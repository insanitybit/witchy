use wasmtime::{Caller, Error, ExternRef, Linker, Result, Rooted};
use std::sync::Arc;

#[path = "exec.rs"]
pub(in crate::runtime) mod canonical_exec;

use crate::confine::{ConfineError, ConfineErrorKind};

use super::cap_response::{self, Minted};
use super::super::{
    confine, memory_of, read_slice_str, read_wstr, read_wstr_list,
    DirAuthority, DirBacking, ExecAuthority, FileAuthority, FileBacking, FsRights, VmState,
};

/// Register the root `Dir` grant mint.
pub(in crate::runtime) fn link_mint_dir(linker: &mut Linker<VmState>) -> Result<()> {
    linker.func_wrap("witchy", "mint_dir", host_mint_dir)?;
    // Narrowing never widens, so it links under any Dir grant.
    linker.func_wrap("witchy", "dir_only_at", host_dir_only_at)?;
    Ok(())
}

/// Register the Dir read family under a `Dir[Read]` grant: a program without
/// the right cannot even link the import.
pub(in crate::runtime) fn link_dir_read(linker: &mut Linker<VmState>) -> Result<()> {
    linker.func_wrap("witchy", "dir_only", host_dir_only)?;
    linker.func_wrap("witchy", "dir_list_at", host_dir_list_at)?;
    linker.func_wrap("witchy", "dir_exists_at", host_dir_exists_at)?;
    linker.func_wrap("witchy", "dir_is_dir_at", host_dir_is_dir_at)?;
    linker.func_wrap("witchy", "dir_subtree_open_at", host_dir_subtree_open_at)?;
    linker.func_wrap("witchy", "dir_read_file_at", host_dir_read_file_at)?;
    linker.func_wrap("witchy", "dir_read_bytes_at", host_dir_read_bytes_at)?;
    linker.func_wrap("witchy", "dir_read_at", host_dir_read_at)?;
    Ok(())
}

/// Register the Dir write family under a `Dir[Write]` grant.
pub(in crate::runtime) fn link_dir_write(linker: &mut Linker<VmState>) -> Result<()> {
    linker.func_wrap("witchy", "dir_write_file_at", host_dir_write_file_at)?;
    linker.func_wrap("witchy", "dir_append_at", host_dir_append_at)?;
    linker.func_wrap("witchy", "dir_append_bytes_at", host_dir_append_bytes_at)?;
    linker.func_wrap("witchy", "dir_set_executable_at", host_dir_set_executable_at)?;
    linker.func_wrap("witchy", "dir_rename_at", host_dir_rename_at)?;
    linker.func_wrap("witchy", "dir_write_at", host_dir_write_at)?;
    linker.func_wrap("witchy", "dir_make_dir_at", host_dir_make_dir_at)?;
    linker.func_wrap("witchy", "dir_create_new_at", host_dir_create_new_at)?;
    linker.func_wrap("witchy", "dir_replace_at", host_dir_replace_at)?;
    linker.func_wrap("witchy", "dir_write_bytes_at", host_dir_write_bytes_at)?;
    Ok(())
}

/// Register the File read leaf op (RFC-0012).
pub(in crate::runtime) fn link_file_read(linker: &mut Linker<VmState>) -> Result<()> {
    linker.func_wrap("witchy", "file_read_at", host_file_read_at)?;
    Ok(())
}

/// Register the File write leaf op (RFC-0012).
pub(in crate::runtime) fn link_file_write(linker: &mut Linker<VmState>) -> Result<()> {
    linker.func_wrap("witchy", "file_write_at", host_file_write_at)?;
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
    confine(dir_admits_entry(dir, name, is_dir))
}

/// `dir_guard`'s judgment as a typed denial.
fn dir_admits_entry(dir: &DirAuthority, name: &str, is_dir: bool) -> Result<(), ConfineError> {
    if witchy_caps::capabilities::dir_admits(&dir.policy, name, is_dir) {
        Ok(())
    } else {
        Err(ConfineError::denied(format!(
            "`{name}` is not permitted by this Dir capability's entry policy"
        )))
    }
}

fn host_mint_dir(mut caller: Caller<'_, VmState>, i: i32) -> Result<Option<Rooted<ExternRef>>> {
    let dir = dir_grant_by_ordinal(&caller, i)?;
    ExternRef::new(&mut caller, dir).map(Some)
}

/// Bind `rel` to a confined file, failing with a typed kind: an alias is a
/// denial, a directory is not a file.
fn dir_file_access(dir: &DirAuthority, rel: &str, write: bool) -> Result<FileBacking, ConfineError> {
    match &dir.backing {
        DirBacking::Fs(base) => {
            if dir.policy.is_empty() {
                return Ok(FileBacking::Fs(base.file(rel, !write)?));
            }
            match base.entry_kind_exact(rel)? {
                Some(crate::confine::ConfinedEntryKind::File) => {}
                None if write => {}
                None => return Ok(FileBacking::Fs(base.file(rel, true)?.without_following())),
                Some(crate::confine::ConfinedEntryKind::Directory) => {
                    return Err(ConfineError::new(
                        ConfineErrorKind::NotAFile,
                        format!("`{rel}` is a directory, not a file"),
                    ));
                }
                Some(crate::confine::ConfinedEntryKind::Other) => {
                    return Err(ConfineError::denied(format!(
                        "`{rel}` is a filesystem alias or unsupported entry"
                    )));
                }
            }
            Ok(FileBacking::Fs(base.file(rel, false)?.without_following()))
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

/// `dir_only_at(d, policy) -> Dir`: `dir.only(policy)`, a Dir whose entry
/// policy is the current one narrowed by `policy`. Refinement only shrinks (a
/// malformed or disjoint one denies everything), so it needs no right and never
/// fails; the rights and backing are the receiver's.
fn host_dir_only_at(
    mut caller: Caller<'_, VmState>, d: Option<Rooted<ExternRef>>, policy_ptr: i32, policy_len: i32,
) -> Result<Option<Rooted<ExternRef>>> {
    let dir = dir_authority_ref(&caller, d)?;
    let memory = memory_of(&mut caller)?;
    let refine = read_slice_str(memory.data(&caller), policy_ptr, policy_len)?;
    let policy = witchy_caps::capabilities::dir_only(&dir.policy, &refine);
    ExternRef::new(&mut caller, DirAuthority { policy, ..dir }).map(Some)
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




// --- (RFC-0165 slice 42) the canonical Dir family ---
//
// Same authority, same confinement, same entry policy as the frozen ops above —
// only the buffer protocol differs. A path arrives as a raw checked `(ptr, len)`
// slice rather than a length-prefixed object, and a read writes a BOUNDED prefix
// into the guest's own buffer instead of staging an unbounded payload in host
// state for a second call to place. Nothing here touches `pending`, so no host
// state survives a canonical call.



#[cfg(feature = "self-checks")]
#[doc(hidden)]
#[path = "filesystem_metadata_tests.rs"]
pub mod metadata_tests;



fn dir_bytes_length(length: usize) -> Result<i32> {
    i32::try_from(length).map_err(|_| Error::msg("Dir response exceeds the guest ABI size limit"))
}

fn dir_storage_range(ptr: i32, length: i32, limit: usize) -> Result<std::ops::Range<usize>> {
    let start = usize::try_from(ptr).map_err(|_| Error::msg("negative Dir storage slice pointer"))?;
    let length = usize::try_from(length).map_err(|_| Error::msg("negative Dir storage slice length"))?;
    let end = start.checked_add(length).filter(|end| *end <= limit)
        .ok_or_else(|| Error::msg("Dir storage slice lies outside guest memory"))?;
    Ok(start..end)
}

fn host_dir_read_bytes_at(
    caller: Caller<'_, VmState>, dir: Option<Rooted<ExternRef>>, path_ptr: i32, path_len: i32,
) -> Result<Option<Rooted<ExternRef>>> {
    dir_read_response(caller, dir, path_ptr, path_len, false)
}

/// `dir_read_at(dir, path, path_len) -> response`: read the confined file
/// once. Rights are checked first and a missing right or forged token still
/// traps; a request the grant does not cover, and every filesystem failure,
/// is the response's status and message (RFC-0126 rule 3).
fn host_dir_read_at(
    caller: Caller<'_, VmState>, dir: Option<Rooted<ExternRef>>, path_ptr: i32, path_len: i32,
) -> Result<Option<Rooted<ExternRef>>> {
    dir_read_response(caller, dir, path_ptr, path_len, true)
}

/// One confined read answered as a response (RFC-0126 rule 3). Rights are
/// checked first and a missing right or forged token still traps; a request
/// the grant does not cover, and every filesystem failure, is the response's
/// status and a guest-safe message. Later getters only borrow this snapshot.
fn dir_read_response(
    mut caller: Caller<'_, VmState>, dir: Option<Rooted<ExternRef>>, path_ptr: i32, path_len: i32, text: bool,
) -> Result<Option<Rooted<ExternRef>>> {
    let dir = dir_authority_ref(&caller, dir)?;
    dir_require_read(&dir)?;
    let memory = memory_of(&mut caller)?;
    let path = read_slice_str(memory.data(&caller), path_ptr, path_len)?;
    let read = dir_read_contents(&dir, &path)
        .and_then(|bytes| if text { utf8_contents(bytes, &path).map(String::into_bytes) } else { Ok(bytes) });
    let (status, bytes) = match read {
        Ok(bytes) => (0, bytes),
        Err(failure) => (failure.kind.status(), guest_failure(failure.kind, &path).into_bytes()),
    };
    dir_bytes_length(bytes.len())?;
    cap_response::respond(&mut caller, status, bytes, None)
}

/// `file_read_at(file) -> response`: `file.read()` as one confined read
/// (RFC-0126 rule 3). A missing `Read` right or a forged token traps; a
/// filesystem failure is the response's status and a fixed message. A
/// `File` names no path the guest supplied, so the message names none.
fn host_file_read_at(mut caller: Caller<'_, VmState>, f: Option<Rooted<ExternRef>>) -> Result<Option<Rooted<ExternRef>>> {
    let file = file_authority_ref(&caller, f)?;
    if !file.rights.read {
        return Err(Error::msg("this File capability does not grant Read"));
    }
    let FileBacking::Fs(backing) = &file.backing;
    let read = backing.read_bytes_classified()
        .and_then(|bytes| utf8_contents(bytes, "File").map(String::into_bytes));
    let (status, bytes) = match read {
        Ok(bytes) => (0, bytes),
        Err(failure) => (failure.kind.status(), format!("this File {}", failure_reason(failure.kind)).into_bytes()),
    };
    dir_bytes_length(bytes.len())?;
    cap_response::respond(&mut caller, status, bytes, None)
}

/// `dir.read` and `dir.read_bytes`'s classification (RFC-0126 rule 3). The guest can branch on
/// the answer, so no answer may depend on anything the capability cannot
/// observe:
///
/// - an absolute or `..` path, and a name the entry policy refuses, are
///   `Denied` before any filesystem access;
/// - under an entry policy, an entry the policy cannot see (an alias or a
///   path through one, a case-folded spelling, a directory a `files()`
///   handle cannot open) is `NotFound`, exactly like a missing name, as
///   `dir.exists` answers `false`;
/// - without a policy, a read through a symlink (the leaf or any parent
///   component) that fails is `Denied` whatever the target is, so a link
///   leaving the root never reports whether its outside target exists.
/// The bytes a guest read decodes as text, or `Io` when they are not UTF-8.
fn utf8_contents(bytes: Vec<u8>, what: &str) -> Result<String, ConfineError> {
    String::from_utf8(bytes).map_err(|error| {
        ConfineError::new(ConfineErrorKind::Io, format!("`{what}` is not UTF-8: {error}"))
    })
}

fn dir_read_contents(dir: &DirAuthority, rel: &str) -> Result<Vec<u8>, ConfineError> {
    use crate::confine::{ConfinedDir, ConfinedEntryKind};
    let DirBacking::Fs(base) = &dir.backing;
    ConfinedDir::check_relative(rel)?;
    dir_admits_entry(dir, rel, false)?;
    let invisible = || ConfineError::new(ConfineErrorKind::NotFound, format!("`{rel}` is not visible"));
    // The shallowest symlink on the path always has a real parent inside the
    // root, so this probe never resolves anything outside it.
    let through_link = std::path::Path::new(rel).ancestors().filter_map(|prefix| prefix.to_str()).any(|prefix| {
        !prefix.is_empty()
            && matches!(base.entry_kind_spelled(prefix), Ok(Some(ConfinedEntryKind::Other)))
    });
    if !dir.policy.is_empty() {
        // Every parent must be an exactly spelled directory; anything else (a
        // file, an alias, a missing name) is invisible, so a probe through it
        // never reports what it hit. This matches `dir.exists`.
        let parents_visible = std::path::Path::new(rel)
            .ancestors()
            .skip(1)
            .filter_map(|prefix| prefix.to_str())
            .filter(|prefix| !prefix.is_empty())
            .all(|prefix| {
                matches!(base.entry_kind_spelled(prefix), Ok(Some(ConfinedEntryKind::Directory)))
            });
        if through_link || !parents_visible {
            return Err(invisible());
        }
        return match base.entry_kind_spelled(rel)? {
            Some(ConfinedEntryKind::File) => {
                let file = base.file(rel, false)?;
                file.read_bytes_no_follow().map_err(|error| ConfineError::new(
                    ConfineErrorKind::of_io(&error),
                    format!("read failed for `{rel}`: {error}"),
                ))
            }
            Some(ConfinedEntryKind::Directory)
                if witchy_caps::capabilities::dir_admits(&dir.policy, rel, true) =>
            {
                Err(ConfineError::new(ConfineErrorKind::NotAFile, format!("`{rel}` is a directory")))
            }
            Some(_) | None => Err(invisible()),
        };
    }
    base.file(rel, true)
        .and_then(|file| file.read_bytes().map_err(|error| ConfineError::new(
            ConfineErrorKind::of_io(&error),
            format!("read failed for `{rel}`: {error}"),
        )))
        .map_err(|failure| if through_link {
            ConfineError::denied(format!("`{rel}` is read through an alias that cannot be read: {failure}"))
        } else {
            failure
        })
}

/// Where a guest path leads under `dir`, decided before the operation runs:
/// whether any component (leaf or parent) is a symlink or special entry, and
/// whether every parent is an exactly spelled directory. Each probe resolves
/// only a prefix's own no-follow identity inside the root.
fn path_shape(dir: &DirAuthority, rel: &str) -> (bool, bool) {
    use crate::confine::ConfinedEntryKind;
    let DirBacking::Fs(base) = &dir.backing;
    let prefixes: Vec<&str> = std::path::Path::new(rel).ancestors()
        .filter_map(|prefix| prefix.to_str())
        .filter(|prefix| !prefix.is_empty())
        .collect();
    let through_link = prefixes.iter().any(|prefix| {
        matches!(base.entry_kind_spelled(prefix), Ok(Some(ConfinedEntryKind::Other)))
    });
    let parents_visible = prefixes.iter().skip(1).all(|prefix| {
        matches!(base.entry_kind_spelled(prefix), Ok(Some(ConfinedEntryKind::Directory)))
    });
    (through_link, parents_visible)
}

/// What a navigation (`subtree`, `read_file`, `write_file`) mints.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Navigation {
    Subtree,
    ReadFile,
    WriteFile,
}

/// Navigate `dir` to `rel` (RFC-0126 rule 3), with `dir.read`'s rules: an
/// escape or policy refusal is `Denied` before any filesystem access; under
/// a policy an entry the policy cannot see answers like a missing one, and a
/// minted File never follows its leaf; without a policy a failure through a
/// symlink is `Denied` whatever its target is. A directory where a file is
/// expected is `NotAFile`; a file where a directory is expected is `Io`.
fn dir_navigate(dir: &DirAuthority, rel: &str, to: Navigation) -> Result<Minted, ConfineError> {
    use crate::confine::{ConfinedDir, ConfinedEntryKind};
    let DirBacking::Fs(base) = &dir.backing;
    ConfinedDir::check_relative(rel)?;
    dir_admits_entry(dir, rel, to == Navigation::Subtree)?;
    let invisible = || ConfineError::new(ConfineErrorKind::NotFound, format!("`{rel}` is not visible"));
    let not_a_file = || ConfineError::new(ConfineErrorKind::NotAFile, format!("`{rel}` is a directory"));
    let not_a_dir = || ConfineError::new(ConfineErrorKind::Io, format!("`{rel}` is not a directory"));
    let file = |backing| Minted::File(FileAuthority {
        backing,
        rights: FsRights::new(to == Navigation::ReadFile, to == Navigation::WriteFile),
    });
    let (through_link, parents_visible) = path_shape(dir, rel);
    if !dir.policy.is_empty() {
        if through_link || !parents_visible {
            return Err(invisible());
        }
        let leaf = base.entry_kind_spelled(rel)?;
        let admitted_dir = witchy_caps::capabilities::dir_admits(&dir.policy, rel, true);
        return match (to, leaf) {
            (Navigation::Subtree, Some(ConfinedEntryKind::Directory)) => Ok(Minted::Dir(DirAuthority {
                backing: DirBacking::Fs(base.open_dir(rel)?),
                policy: dir.policy.clone(),
                rights: dir.rights,
            })),
            // A file is visible only when the policy admits it as a file.
            (Navigation::Subtree, Some(ConfinedEntryKind::File))
                if witchy_caps::capabilities::dir_admits(&dir.policy, rel, false) => Err(not_a_dir()),
            (Navigation::Subtree, _) => Err(invisible()),
            (_, Some(ConfinedEntryKind::Directory)) if admitted_dir => Err(not_a_file()),
            (Navigation::ReadFile, Some(ConfinedEntryKind::File)) => {
                Ok(file(FileBacking::Fs(base.file(rel, false)?.without_following())))
            }
            (Navigation::ReadFile, _) => Err(invisible()),
            // A write names a file to create or replace; it is not judged by
            // what occupies the name, and its writes never follow a leaf.
            (Navigation::WriteFile, _) => Ok(file(FileBacking::Fs(base.file(rel, false)?.without_following()))),
        };
    }
    let through = |failure: ConfineError| if through_link {
        ConfineError::denied(format!("`{rel}` is reached through an alias: {failure}"))
    } else {
        failure
    };
    match to {
        Navigation::Subtree => {
            let opened = base.open_dir(rel).map_err(through)?;
            Ok(Minted::Dir(DirAuthority { backing: DirBacking::Fs(opened), policy: dir.policy.clone(), rights: dir.rights }))
        }
        Navigation::ReadFile | Navigation::WriteFile => {
            let opened = base.file(rel, to == Navigation::ReadFile).map_err(through)?;
            if base.is_dir(rel).unwrap_or(false) {
                return Err(not_a_file());
            }
            Ok(file(FileBacking::Fs(opened)))
        }
    }
}

/// One navigation answered as a response that carries the minted capability
/// on success. Rights are checked first and still trap.
fn dir_navigation_response(
    mut caller: Caller<'_, VmState>, d: Option<Rooted<ExternRef>>, path_ptr: i32, path_len: i32, to: Navigation,
) -> Result<Option<Rooted<ExternRef>>> {
    let dir = dir_authority_ref(&caller, d)?;
    if to == Navigation::WriteFile { dir_require_write(&dir)?; } else { dir_require_read(&dir)?; }
    let memory = memory_of(&mut caller)?;
    let path = read_slice_str(memory.data(&caller), path_ptr, path_len)?;
    let (status, bytes, minted) = match dir_navigate(&dir, &path, to) {
        Ok(minted) => (0, Vec::new(), Some(minted)),
        Err(failure) => (failure.kind.status(), guest_open_failure(failure.kind, &path).into_bytes(), None),
    };
    cap_response::respond(&mut caller, status, bytes, minted)
}

/// `dir_subtree_open_at(dir, path) -> response`: `dir.subtree(path)`.
fn host_dir_subtree_open_at(
    caller: Caller<'_, VmState>, d: Option<Rooted<ExternRef>>, path_ptr: i32, path_len: i32,
) -> Result<Option<Rooted<ExternRef>>> {
    dir_navigation_response(caller, d, path_ptr, path_len, Navigation::Subtree)
}

/// `dir_read_file_at(dir, path) -> response`: `dir.read_file(path)`.
fn host_dir_read_file_at(
    caller: Caller<'_, VmState>, d: Option<Rooted<ExternRef>>, path_ptr: i32, path_len: i32,
) -> Result<Option<Rooted<ExternRef>>> {
    dir_navigation_response(caller, d, path_ptr, path_len, Navigation::ReadFile)
}

/// `dir_write_file_at(dir, path) -> response`: `dir.write_file(path)`.
fn host_dir_write_file_at(
    caller: Caller<'_, VmState>, d: Option<Rooted<ExternRef>>, path_ptr: i32, path_len: i32,
) -> Result<Option<Rooted<ExternRef>>> {
    dir_navigation_response(caller, d, path_ptr, path_len, Navigation::WriteFile)
}

/// Where a write may land (RFC-0126 rule 3). An escape or a name the policy
/// refuses is `Denied` before any filesystem access. Under a policy, a path
/// through or onto an entry the policy cannot see (a link, an alias, a
/// case-folded spelling, or an entry of a kind the policy hides) answers
/// `NotFound`, as `write_file` does: the write never lands on it and its kind
/// is not observable. A visible directory where a file is written is
/// `NotAFile`.
/// Returns whether the path passes through a symlink, which makes any later
/// failure `Denied` whatever the target is.
fn dir_write_target(dir: &DirAuthority, rel: &str, is_dir: bool) -> Result<bool, ConfineError> {
    use crate::confine::{ConfinedDir, ConfinedEntryKind};
    let DirBacking::Fs(base) = &dir.backing;
    ConfinedDir::check_relative(rel)?;
    dir_admits_entry(dir, rel, is_dir)?;
    let (through_link, parents_visible) = path_shape(dir, rel);
    if !dir.policy.is_empty() && (through_link || !parents_visible) {
        return Err(ConfineError::new(ConfineErrorKind::NotFound, format!("`{rel}` is not visible")));
    }
    if through_link {
        return Ok(true);
    }
    let occupant = match base.entry_kind_spelled(rel) {
        Ok(Some(ConfinedEntryKind::Directory)) => Some(true),
        Ok(Some(ConfinedEntryKind::File)) => Some(false),
        _ => None,
    };
    if let Some(occupied_by_dir) = occupant {
        // An occupant the policy cannot see answers like an empty name would
        // under a missing parent: its kind is not observable.
        if !witchy_caps::capabilities::dir_admits(&dir.policy, rel, occupied_by_dir) {
            return Err(ConfineError::new(ConfineErrorKind::NotFound, format!("`{rel}` is not visible")));
        }
        if occupied_by_dir && !is_dir {
            return Err(ConfineError::new(ConfineErrorKind::NotAFile, format!("`{rel}` is a directory")));
        }
    }
    Ok(false)
}

/// A filesystem failure after `dir_write_target` admitted the path: through a
/// symlink it is `Denied` whatever the target is, otherwise its own kind.
fn after_link(through_link: bool, rel: &str, failure: ConfineError) -> ConfineError {
    if through_link {
        ConfineError::denied(format!("`{rel}` is reached through an alias: {failure}"))
    } else {
        failure
    }
}

fn io_failure(rel: &str, error: std::io::Error) -> ConfineError {
    ConfineError::new(ConfineErrorKind::of_io(&error), format!("write failed for `{rel}`: {error}"))
}

/// Write (or append) `bytes` to the confined file `rel`, never following its leaf.
fn dir_write_bytes_typed(dir: &DirAuthority, rel: &str, bytes: &[u8], append: bool) -> Result<(), ConfineError> {
    let through = dir_write_target(dir, rel, false)?;
    let wrote = (|| {
        let FileBacking::Fs(file) = dir_file_access(dir, rel, true)?;
        let written = if append { file.append_all(bytes) } else { file.write_all(bytes) };
        written.map_err(|error| io_failure(rel, error))
    })();
    wrote.map_err(|failure| after_link(through, rel, failure))
}

/// One write-family operation answered as a response (RFC-0126 rule 3): a
/// status, a guest-safe message naming `rel`, and `flag` for a predicate.
fn dir_effect_response(
    caller: &mut Caller<'_, VmState>, rel: &str, outcome: Result<bool, ConfineError>,
) -> Result<Option<Rooted<ExternRef>>> {
    let (status, bytes, flag) = match outcome {
        Ok(flag) => (0, Vec::new(), flag),
        Err(failure) => (failure.kind.status(), guest_write_failure(failure.kind, rel).into_bytes(), false),
    };
    cap_response::respond_flag(caller, status, bytes, flag)
}

/// The guest-visible message of a failed write-family operation on `rel`.
fn guest_write_failure(kind: ConfineErrorKind, rel: &str) -> String {
    match kind {
        ConfineErrorKind::Io => format!("`{rel}` could not be written"),
        ConfineErrorKind::NotFound => format!("`{rel}` was not found (or its parent was not)"),
        _ => guest_failure(kind, rel),
    }
}

/// Authenticate a write-family call and read its path (and optional data)
/// slices; a missing right, forged token, or bad slice traps.
fn dir_write_inputs(
    caller: &mut Caller<'_, VmState>, d: Option<Rooted<ExternRef>>,
    path_ptr: i32, path_len: i32, data: Option<(i32, i32)>,
) -> Result<(DirAuthority, String, Vec<u8>)> {
    let dir = dir_authority_ref(caller, d)?;
    dir_require_write(&dir)?;
    let memory = memory_of(caller)?;
    let size = memory.data_size(&*caller);
    dir_storage_range(path_ptr, path_len, size)?;
    let path = read_slice_str(memory.data(&*caller), path_ptr, path_len)?;
    let bytes = match data {
        Some((ptr, len)) => memory.data(&*caller)[dir_storage_range(ptr, len, size)?].to_vec(),
        None => Vec::new(),
    };
    Ok((dir, path, bytes))
}

fn text(bytes: Vec<u8>) -> Result<String> {
    String::from_utf8(bytes).map_err(|error| Error::msg(format!("write data is not valid UTF-8: {error}")))
}

/// `dir_write_at(dir, path, data) -> response`: `dir.write(path, data)`.
fn host_dir_write_at(
    mut caller: Caller<'_, VmState>, d: Option<Rooted<ExternRef>>,
    path_ptr: i32, path_len: i32, data_ptr: i32, data_len: i32,
) -> Result<Option<Rooted<ExternRef>>> {
    let (dir, path, bytes) = dir_write_inputs(&mut caller, d, path_ptr, path_len, Some((data_ptr, data_len)))?;
    let contents = text(bytes)?;
    let outcome = dir_write_bytes_typed(&dir, &path, contents.as_bytes(), false).map(|()| false);
    dir_effect_response(&mut caller, &path, outcome)
}

/// `dir_append_at(dir, path, data) -> response`: `dir.append(path, data)`.
fn host_dir_append_at(
    mut caller: Caller<'_, VmState>, d: Option<Rooted<ExternRef>>,
    path_ptr: i32, path_len: i32, data_ptr: i32, data_len: i32,
) -> Result<Option<Rooted<ExternRef>>> {
    let (dir, path, bytes) = dir_write_inputs(&mut caller, d, path_ptr, path_len, Some((data_ptr, data_len)))?;
    let contents = text(bytes)?;
    let outcome = dir_write_bytes_typed(&dir, &path, contents.as_bytes(), true).map(|()| false);
    dir_effect_response(&mut caller, &path, outcome)
}

/// `dir_write_bytes_at(dir, path, data) -> response`.
fn host_dir_write_bytes_at(
    mut caller: Caller<'_, VmState>, d: Option<Rooted<ExternRef>>,
    path_ptr: i32, path_len: i32, data_ptr: i32, data_len: i32,
) -> Result<Option<Rooted<ExternRef>>> {
    let (dir, path, bytes) = dir_write_inputs(&mut caller, d, path_ptr, path_len, Some((data_ptr, data_len)))?;
    let outcome = dir_write_bytes_typed(&dir, &path, &bytes, false).map(|()| false);
    dir_effect_response(&mut caller, &path, outcome)
}

/// `dir_append_bytes_at(dir, path, data) -> response`.
fn host_dir_append_bytes_at(
    mut caller: Caller<'_, VmState>, d: Option<Rooted<ExternRef>>,
    path_ptr: i32, path_len: i32, data_ptr: i32, data_len: i32,
) -> Result<Option<Rooted<ExternRef>>> {
    let (dir, path, bytes) = dir_write_inputs(&mut caller, d, path_ptr, path_len, Some((data_ptr, data_len)))?;
    let outcome = dir_write_bytes_typed(&dir, &path, &bytes, true).map(|()| false);
    dir_effect_response(&mut caller, &path, outcome)
}

/// `dir_create_new_at(dir, path, data) -> response`: create `path` only if
/// nothing occupies the name; an occupied visible name is `AlreadyExists`.
fn host_dir_create_new_at(
    mut caller: Caller<'_, VmState>, d: Option<Rooted<ExternRef>>,
    path_ptr: i32, path_len: i32, data_ptr: i32, data_len: i32,
) -> Result<Option<Rooted<ExternRef>>> {
    let (dir, path, bytes) = dir_write_inputs(&mut caller, d, path_ptr, path_len, Some((data_ptr, data_len)))?;
    let contents = text(bytes)?;
    let outcome = (|| {
        let through = dir_write_target(&dir, &path, false)?;
        let DirBacking::Fs(base) = &dir.backing;
        match base.create_new(&path, contents.as_bytes()) {
            Ok(true) => Ok(false),
            Ok(false) if through => Err(ConfineError::denied(format!("`{path}` is an alias"))),
            Ok(false) => Err(ConfineError::new(ConfineErrorKind::AlreadyExists, format!("`{path}` already exists"))),
            Err(failure) => Err(after_link(through, &path, failure)),
        }
    })();
    dir_effect_response(&mut caller, &path, outcome)
}

/// `dir_replace_at(dir, path, data) -> response`: atomically replace `path`.
fn host_dir_replace_at(
    mut caller: Caller<'_, VmState>, d: Option<Rooted<ExternRef>>,
    path_ptr: i32, path_len: i32, data_ptr: i32, data_len: i32,
) -> Result<Option<Rooted<ExternRef>>> {
    let (dir, path, bytes) = dir_write_inputs(&mut caller, d, path_ptr, path_len, Some((data_ptr, data_len)))?;
    let contents = text(bytes)?;
    let outcome = (|| {
        let through = dir_write_target(&dir, &path, false)?;
        let DirBacking::Fs(base) = &dir.backing;
        base.replace(&path, contents.as_bytes()).map_err(|failure| after_link(through, &path, failure))
    })().map(|()| false);
    dir_effect_response(&mut caller, &path, outcome)
}

/// `dir_set_executable_at(dir, path) -> response`.
fn host_dir_set_executable_at(
    mut caller: Caller<'_, VmState>, d: Option<Rooted<ExternRef>>, path_ptr: i32, path_len: i32,
) -> Result<Option<Rooted<ExternRef>>> {
    let (dir, path, _) = dir_write_inputs(&mut caller, d, path_ptr, path_len, None)?;
    let outcome = (|| {
        let through = dir_write_target(&dir, &path, false)?;
        let changed = (|| {
            let FileBacking::Fs(file) = dir_file_access(&dir, &path, false)?;
            file.set_executable().map_err(|error| io_failure(&path, error))
        })();
        changed.map_err(|failure| after_link(through, &path, failure))
    })().map(|()| false);
    dir_effect_response(&mut caller, &path, outcome)
}

/// `dir_make_dir_at(dir, path) -> response`: create the directory path
/// (idempotent).
fn host_dir_make_dir_at(
    mut caller: Caller<'_, VmState>, d: Option<Rooted<ExternRef>>, path_ptr: i32, path_len: i32,
) -> Result<Option<Rooted<ExternRef>>> {
    let (dir, path, _) = dir_write_inputs(&mut caller, d, path_ptr, path_len, None)?;
    let outcome = (|| {
        let through = dir_write_target(&dir, &path, true)?;
        let DirBacking::Fs(base) = &dir.backing;
        base.make_dir(&path).map_err(|failure| after_link(through, &path, failure))
    })().map(|()| false);
    dir_effect_response(&mut caller, &path, outcome)
}

/// `dir_rename_at(dir, from, to) -> response`: both names are judged as a
/// write target of the source's kind; a source the grant cannot see is
/// `NotFound`, exactly like a missing one.
fn host_dir_rename_at(
    mut caller: Caller<'_, VmState>, d: Option<Rooted<ExternRef>>,
    from_ptr: i32, from_len: i32, to_ptr: i32, to_len: i32,
) -> Result<Option<Rooted<ExternRef>>> {
    let (dir, from, to_bytes) = dir_write_inputs(&mut caller, d, from_ptr, from_len, Some((to_ptr, to_len)))?;
    let to = text(to_bytes)?;
    // A failure judged on the destination names the destination.
    let source = |failure| (false, failure);
    let target = |failure| (true, failure);
    let outcome = (|| -> Result<(), (bool, ConfineError)> {
        use crate::confine::ConfinedEntryKind;
        let DirBacking::Fs(base) = &dir.backing;
        crate::confine::ConfinedDir::check_relative(&from).map_err(source)?;
        crate::confine::ConfinedDir::check_relative(&to).map_err(target)?;
        let source_is_dir = if dir.policy.is_empty() {
            false
        } else if !witchy_caps::capabilities::dir_admits(&dir.policy, &from, false)
            && !witchy_caps::capabilities::dir_admits(&dir.policy, &from, true)
        {
            // A name the policy refuses as any kind is Denied, present or not.
            return Err(source(ConfineError::denied(format!("`{from}` is not permitted by this Dir capability's entry policy"))));
        } else {
            let (through, parents_visible) = path_shape(&dir, &from);
            let kind = if through || !parents_visible { None } else { base.entry_kind_spelled(&from).ok().flatten() };
            match kind {
                Some(ConfinedEntryKind::File) => Some(false),
                Some(ConfinedEntryKind::Directory) => Some(true),
                _ => None,
            }.filter(|is_dir| witchy_caps::capabilities::dir_admits(&dir.policy, &from, *is_dir))
                .ok_or_else(|| source(ConfineError::new(ConfineErrorKind::NotFound, format!("`{from}` is not visible"))))?
        };
        dir_admits_entry(&dir, &from, source_is_dir).map_err(source)?;
        let through_from = path_shape(&dir, &from).0;
        let through_to = dir_write_target(&dir, &to, source_is_dir).map_err(target)?;
        base.rename(&from, &to).map_err(|failure| {
            // The source was judged present above (or is checked here), so a
            // failure past it is the destination's.
            let blame_target = !through_from
                && (through_to || matches!(base.entry_kind_spelled(&from), Ok(Some(_))));
            let rel = if blame_target { &to } else { &from };
            (blame_target, after_link(through_from || through_to, rel, failure))
        })
    })();
    match outcome {
        Ok(()) => dir_effect_response(&mut caller, &from, Ok(false)),
        Err((true, failure)) => dir_effect_response(&mut caller, &to, Err(failure)),
        Err((false, failure)) => dir_effect_response(&mut caller, &from, Err(failure)),
    }
}

/// `dir_exists_at(dir, path) -> response`: `dir.exists(path)`. An escape or a
/// name the policy refuses is `Denied`; otherwise the flag is whether the
/// grant can see an entry there, so an invisible entry answers `false`.
fn host_dir_exists_at(
    mut caller: Caller<'_, VmState>, d: Option<Rooted<ExternRef>>, path_ptr: i32, path_len: i32,
) -> Result<Option<Rooted<ExternRef>>> {
    dir_probe_response(&mut caller, d, path_ptr, path_len, false)
}

/// `dir_is_dir_at(dir, path) -> response`: `dir.is_dir(path)`, as
/// `dir_exists_at` but true only for a visible directory.
fn host_dir_is_dir_at(
    mut caller: Caller<'_, VmState>, d: Option<Rooted<ExternRef>>, path_ptr: i32, path_len: i32,
) -> Result<Option<Rooted<ExternRef>>> {
    dir_probe_response(&mut caller, d, path_ptr, path_len, true)
}

fn dir_probe_response(
    caller: &mut Caller<'_, VmState>, d: Option<Rooted<ExternRef>>, path_ptr: i32, path_len: i32, directory: bool,
) -> Result<Option<Rooted<ExternRef>>> {
    let dir = dir_authority_ref(caller, d)?;
    dir_require_read(&dir)?;
    let memory = memory_of(caller)?;
    let rel = read_slice_str(memory.data(&*caller), path_ptr, path_len)?;
    let outcome = (|| {
        crate::confine::ConfinedDir::check_relative(&rel)?;
        if !dir.policy.is_empty()
            && !witchy_caps::capabilities::dir_admits(&dir.policy, &rel, false)
            && !witchy_caps::capabilities::dir_admits(&dir.policy, &rel, true)
        {
            return Err(ConfineError::denied(format!("`{rel}` is not permitted by this Dir capability's entry policy")));
        }
        if dir.policy.is_empty() {
            // A link that leaves the root fails to resolve whatever its target
            // is; like a read through it, that is `Denied`.
            let DirBacking::Fs(base) = &dir.backing;
            let found = base.exists(&rel).map_err(|failure| after_link(path_shape(&dir, &rel).0, &rel, failure))?;
            return Ok(found && (!directory || base.is_dir(&rel).unwrap_or(false)));
        }
        let (through_link, parents_visible) = path_shape(&dir, &rel);
        if through_link || !parents_visible {
            return Ok(false);
        }
        Ok(match visible_entry_kind(&dir, &rel).ok().flatten() {
            Some(is_dir) => !directory || is_dir,
            None => false,
        })
    })();
    let (status, bytes, flag) = match outcome {
        Ok(flag) => (0, Vec::new(), flag),
        Err(failure) => (failure.kind.status(), guest_failure(failure.kind, &rel).into_bytes(), false),
    };
    cap_response::respond_flag(caller, status, bytes, flag)
}

/// `file_write_at(file, data) -> response`: `file.write(data)` (RFC-0126
/// rule 3). A missing Write right or forged token traps; a filesystem failure
/// is the status with a fixed message that names no path. Writes never follow
/// the leaf.
fn host_file_write_at(
    mut caller: Caller<'_, VmState>, f: Option<Rooted<ExternRef>>, data_ptr: i32, data_len: i32,
) -> Result<Option<Rooted<ExternRef>>> {
    let file = file_authority_ref(&caller, f)?;
    if !file.rights.write {
        return Err(Error::msg("this File capability does not grant Write"));
    }
    let memory = memory_of(&mut caller)?;
    let contents = text(memory.data(&caller)[dir_storage_range(data_ptr, data_len, memory.data_size(&caller))?].to_vec())?;
    let FileBacking::Fs(backing) = &file.backing;
    let (status, bytes) = match backing.write_all(contents.as_bytes()) {
        Ok(()) => (0, Vec::new()),
        Err(error) => {
            let kind = ConfineErrorKind::of_io(&error);
            let reason = match kind { ConfineErrorKind::Io => "could not be written", other => failure_reason(other) };
            (kind.status(), format!("this File {reason}").into_bytes())
        }
    };
    cap_response::respond(&mut caller, status, bytes, None)
}

/// The guest-visible message of a failed navigation on `rel`.
fn guest_open_failure(kind: ConfineErrorKind, rel: &str) -> String {
    match kind {
        ConfineErrorKind::Io => format!("`{rel}` could not be opened"),
        _ => guest_failure(kind, rel),
    }
}

/// The guest-visible message of a failed `Dir` operation on `rel`. A
/// `CapError` message is program data that may leave over any channel the
/// program holds, so it names the path only as the guest spelled it and the
/// failure only by a fixed reason for its kind. The host message (a host
/// path, raw OS error text) never crosses.
fn guest_failure(kind: ConfineErrorKind, rel: &str) -> String {
    let reason = match kind {
        ConfineErrorKind::Denied => "is not covered by this Dir capability",
        _ => failure_reason(kind),
    };
    format!("`{rel}` {reason}")
}

/// The fixed guest-visible reason for a failure kind.
fn failure_reason(kind: ConfineErrorKind) -> &'static str {
    match kind {
        ConfineErrorKind::Denied => "is not covered by its capability",
        ConfineErrorKind::NotFound => "was not found",
        ConfineErrorKind::NotAFile => "is not a file",
        ConfineErrorKind::AlreadyExists => "already exists",
        ConfineErrorKind::Io => "could not be read",
    }
}

#[cfg(feature = "self-checks")]
#[doc(hidden)]
#[path = "dir_storage_tests.rs"]
pub mod dir_storage_tests;

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

/// `dir_list_at(dir) -> response`: the admitted entry names, sorted and
/// NUL-joined (a name cannot contain NUL), read once (RFC-0126 rule 3). A
/// missing right or forged token traps; a directory that cannot be listed is
/// the response's status and a fixed message. Under an entry policy the
/// listing names exactly the entries `dir.exists` reports, so it never
/// reveals an alias or other entry the policy keeps invisible.
fn host_dir_list_at(mut caller: Caller<'_, VmState>, d: Option<Rooted<ExternRef>>) -> Result<Option<Rooted<ExternRef>>> {
    let dir = dir_authority_ref(&caller, d)?;
    dir_require_read(&dir)?;
    let (status, bytes) = match visible_entry_names(&dir) {
        Ok(names) => (0, names.join("\0").into_bytes()),
        Err(failure) => (failure.kind.status(), format!("this Dir {}", list_failure_reason(failure.kind)).into_bytes()),
    };
    dir_bytes_length(bytes.len())?;
    cap_response::respond(&mut caller, status, bytes, None)
}

/// The fixed guest-visible reason a listing failed.
fn list_failure_reason(kind: ConfineErrorKind) -> &'static str {
    match kind {
        ConfineErrorKind::NotFound => "was not found",
        ConfineErrorKind::Denied => "cannot be listed under its capability",
        ConfineErrorKind::NotAFile | ConfineErrorKind::AlreadyExists | ConfineErrorKind::Io => "could not be listed",
    }
}

/// The names `dir.list()` answers. Without a policy every entry, including a
/// symlink (its name only; reading through it is `Denied` when it leaves the
/// root). Under a policy exactly the files and directories the policy admits,
/// judged on their no-follow kinds from the one directory read, so the
/// listing names what `dir.exists` reports and never an alias. Directory
/// reads spell names exactly, so no case-folded alias can appear. A name that
/// is not UTF-8 makes the directory unlistable (`Io`).
fn visible_entry_names(dir: &DirAuthority) -> Result<Vec<String>, ConfineError> {
    use crate::confine::ConfinedEntryKind;
    let DirBacking::Fs(base) = &dir.backing;
    let entries = base.entry_kinds().map_err(|error| ConfineError::new(
        ConfineErrorKind::of_io(&error),
        format!("list failed for `{}`: {error}", base.display_path().display()),
    ))?;
    Ok(entries.into_iter().filter_map(|(name, kind)| {
        let admitted = dir.policy.is_empty() || match kind {
            ConfinedEntryKind::File => witchy_caps::capabilities::dir_admits(&dir.policy, &name, false),
            ConfinedEntryKind::Directory => witchy_caps::capabilities::dir_admits(&dir.policy, &name, true),
            ConfinedEntryKind::Other => false,
        };
        admitted.then_some(name)
    }).collect())
}




#[cfg(feature = "self-checks")]
#[doc(hidden)]
#[path = "dir_installer_tests.rs"]
pub mod dir_installer_tests;


#[cfg(feature = "self-checks")]
#[doc(hidden)]
pub mod tests {
    use super::super::super::{Capabilities, FsRights, Runtime};

    /// Write through the FIRST Dir grant, which the tests below grant read-only.
    const WRITE_AT: &str = r#"
        (module
          (import "witchy" "mint_dir" (func $mint_dir (param i32) (result externref)))
          (import "witchy" "dir_write_at"
            (func $dir_write_at (param externref i32 i32 i32 i32) (result externref)))
          (memory (export "memory") 1)
          (data (i32.const 0) "w.txt")
          (data (i32.const 16) "data")
          (func (export "run")
            (drop (call $dir_write_at (call $mint_dir (i32.const 0))
              (i32.const 0) (i32.const 5) (i32.const 16) (i32.const 4)))))
    "#;

    fn seeded_root(name: &str, contents: &str) -> tempfile::TempDir {
        let root = tempfile::tempdir().expect("granted root");
        std::fs::write(root.path().join(name), contents).expect("seed the granted root");
        root
    }

    /// Rights live in the authority object, so a read-only token refuses the
    /// write even when a sibling grant made the import linkable.
    pub fn dir_write_at_refuses_a_read_only_dir() {
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
    pub fn dir_write_at_is_not_linked_without_a_write_grant() {
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
