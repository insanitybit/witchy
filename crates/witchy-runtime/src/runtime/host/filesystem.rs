use wasmtime::{Caller, Error, ExternRef, Linker, Result, Rooted};

use super::super::{
    confine, memory_of, read_wbytes, read_wstr, read_wstr_list, DirAuthority, DirBacking,
    ExecAuthority, FileAuthority, FileBacking, FsRights, VmState,
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
    Ok(())
}

/// Register the File read leaf op (RFC-0012).
pub(in crate::runtime) fn link_file_read(linker: &mut Linker<VmState>) -> Result<()> {
    linker.func_wrap("witchy", "file_read_len", host_file_read_len)?;
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

fn host_mint_exec(mut caller: Caller<'_, VmState>) -> Result<Option<Rooted<ExternRef>>> {
    let allow = caller
        .data()
        .caps
        .exec_allow
        .as_ref()
        .map(|programs| programs.iter().cloned().collect());
    ExternRef::new(&mut caller, ExecAuthority { allow }).map(Some)
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
    caller
        .data()
        .dirs
        .get(h as usize)
        .cloned()
        .ok_or_else(|| Error::msg(format!("invalid Dir grant index {h}")))
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
            Ok(FileBacking::Fs(confine(base.file(rel, !write))?))
        }
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
    if let Some(allow) = &exec.allow {
        if !allow.contains(&path) {
            return Err(Error::msg(format!("exec: `{path}` is not in this Exec grant's allow-list")));
        }
    }
    let dir = dir_authority_ref(&caller, d)?;
    dir_require_read(&dir)?;
    // (RFC-0011) exec is the sharpest right, so it takes the SAME entry-policy gate as
    // every other Dir op: a `Dir[...].only(...)` may only run a file it admits — "you
    // can only run a file you can read" was false while this was skipped.
    dir_guard(&dir, &path, false)?;
    let DirBacking::Fs(base) = &dir.backing;
    let prog = confine(base.file(&path, true))?;
    let argv: Vec<&str> = if joined.is_empty() { Vec::new() } else { joined.split('\0').collect() };
    let output = prog.run(&argv, &stdin).map_err(|e| {
        Error::msg(format!("exec failed running `{}`: {e}", prog.display_path().display()))
    })?;
    let code = output.status.code().unwrap_or(-1);
    let out = String::from_utf8_lossy(&output.stdout);
    let serr = String::from_utf8_lossy(&output.stderr);
    let payload = format!("{code}\n{out}{serr}");
    let len = payload.len() as i32;
    caller.data_mut().pending = Some(payload.into_bytes());
    Ok(len)
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
    let ok = match &dir.backing {
        DirBacking::Fs(base) => base.exists(&rel).unwrap_or(false),
    };
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
    let ok = match &dir.backing {
        DirBacking::Fs(base) => base.is_dir(&rel).unwrap_or(false),
    };
    Ok(ok as i32)
}

/// `dir_list_size(h) -> bytes`: read the directory NOW (sorted names, matching
/// the interpreter), stage the listing, and report the total byte size of the
/// witchy `List(String)` structure the guest must reserve.
fn host_dir_list_size(mut caller: Caller<'_, VmState>, d: Option<Rooted<ExternRef>>) -> Result<i32> {
    let dir = dir_authority_ref(&caller, d)?;
    dir_require_read(&dir)?;
    let names: Vec<String> = match &dir.backing {
        DirBacking::Fs(base) => base.entries().map_err(|e| {
            Error::msg(format!("list failed for `{}`: {e}", base.display_path().display()))
        })?,
    };
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
/// metadata). The installer calls it after streaming a trusted-exe so the written
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
    dir_guard(&dir, &from, false)?;
    dir_guard(&dir, &to, false)?;
    match &dir.backing {
        DirBacking::Fs(base) => base.rename(&from, &to).map_err(|e| {
            Error::msg(format!("rename failed (`{from}` -> `{to}`): {}", e.0))
        }),
    }
}
