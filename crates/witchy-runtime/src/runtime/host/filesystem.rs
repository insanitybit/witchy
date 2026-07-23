use std::sync::Arc;

use wasmtime::{Caller, Error, ExternRef, Linker, Result, Rooted};

use super::super::{
    confine, memory_of, read_wstr, read_wstr_list, read_wstr_pair_list, DirAuthority,
    DirBacking, ExecAuthority, FileAuthority, FileBacking, FsRights, VmState,
};

/// Register the test-only in-memory Dir mint (RFC-0077). The import grants no
/// host authority; it wraps guest-provided strings in an opaque externref so
/// ordinary `Dir[Read]` code can be tested without real filesystem grants.
/// Source-level link mode decides where `testing.mock_dir` is allowed; the
/// runtime gate is the second line of defense for hosts embedding precompiled
/// artifacts.
pub(in crate::runtime) fn link_mock_dir(linker: &mut Linker<VmState>) -> Result<()> {
    linker.func_wrap("witchy", "testing_mock_dir", host_testing_mock_dir)?;
    Ok(())
}

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
// `resolve`/`resolve_write` confinement the interpreter uses (lexical
// `..`/absolute rejection + symlink-aware canonicalization), so the two backends
// agree on exactly which paths are reachable.

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

fn host_testing_mock_dir(
    mut caller: Caller<'_, VmState>,
    entries_ptr: i32,
) -> Result<Option<Rooted<ExternRef>>> {
    let mem = memory_of(&mut caller)?;
    let entries = read_wstr_pair_list(mem.data(&caller), entries_ptr)?;
    let mut files = std::collections::BTreeMap::new();
    for (path, contents) in entries {
        let path = mock_normalize(&path)?;
        if path.is_empty() {
            return Err(Error::msg("mock Dir entry path must name a file"));
        }
        files.insert(path, contents);
    }
    let dir = DirAuthority {
        backing: DirBacking::Mock {
            root: String::new(),
            files: Arc::new(files),
        },
        policy: String::new(),
        rights: FsRights::new(true, false),
    };
    ExternRef::new(&mut caller, dir).map(Some)
}

fn mock_normalize(rel: &str) -> Result<String> {
    let path = std::path::Path::new(rel);
    if path.is_absolute() {
        return Err(Error::msg(format!("mock Dir path `{rel}` must be relative")));
    }
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::Normal(part) => {
                let Some(part) = part.to_str() else {
                    return Err(Error::msg(format!("mock Dir path `{rel}` is not valid UTF-8")));
                };
                parts.push(part.to_string());
            }
            std::path::Component::ParentDir => {
                return Err(Error::msg(format!("mock Dir path `{rel}` may not contain `..`")));
            }
            std::path::Component::RootDir | std::path::Component::Prefix(_) => {
                return Err(Error::msg(format!("mock Dir path `{rel}` must be relative")));
            }
        }
    }
    Ok(parts.join("/"))
}

fn mock_join(root: &str, rel: &str) -> Result<String> {
    let rel = mock_normalize(rel)?;
    Ok(match (root.is_empty(), rel.is_empty()) {
        (_, true) => root.to_string(),
        (true, false) => rel,
        (false, false) => format!("{root}/{rel}"),
    })
}

fn mock_is_dir(files: &std::collections::BTreeMap<String, String>, path: &str) -> bool {
    if path.is_empty() {
        return true;
    }
    let prefix = format!("{path}/");
    files.keys().any(|entry| entry.starts_with(&prefix))
}

fn mock_exists(files: &std::collections::BTreeMap<String, String>, path: &str) -> bool {
    files.contains_key(path) || mock_is_dir(files, path)
}

fn mock_list(files: &std::collections::BTreeMap<String, String>, root: &str) -> Result<Vec<String>> {
    if !mock_is_dir(files, root) {
        return Err(Error::msg(format!("list failed for mock Dir `{root}`: not a directory")));
    }
    let mut names = std::collections::BTreeSet::new();
    let prefix = if root.is_empty() {
        String::new()
    } else {
        format!("{root}/")
    };
    for path in files.keys() {
        let Some(rest) = path.strip_prefix(&prefix) else {
            continue;
        };
        if rest.is_empty() {
            continue;
        }
        let name = rest.split('/').next().unwrap_or(rest);
        names.insert(name.to_string());
    }
    Ok(names.into_iter().collect())
}

fn dir_child_backing(dir: &DirAuthority, name: &str) -> Result<DirBacking> {
    match &dir.backing {
        DirBacking::Fs(base) => {
            Ok(DirBacking::Fs(confine(crate::confine::resolve(base, name))?))
        }
        DirBacking::Mock { root, files } => {
            Ok(DirBacking::Mock { root: mock_join(root, name)?, files: files.clone() })
        }
    }
}

fn dir_file_backing(dir: &DirAuthority, rel: &str, write: bool) -> Result<FileBacking> {
    match &dir.backing {
        DirBacking::Fs(base) => {
            let path = if write {
                confine(crate::confine::resolve_write(base, rel))?
            } else {
                confine(crate::confine::resolve(base, rel))?
            };
            Ok(FileBacking::Fs(path))
        }
        DirBacking::Mock { root, files } => {
            Ok(FileBacking::Mock { path: mock_join(root, rel)?, files: files.clone() })
        }
    }
}

fn read_file_backing(file: &FileBacking) -> Result<String> {
    match file {
        FileBacking::Fs(path) => {
            use std::io::Read;
            crate::confine::open_read(path)
                .and_then(|mut f| {
                    let mut s = String::new();
                    f.read_to_string(&mut s).map(|_| s)
                })
                .map_err(|e| Error::msg(format!("read failed for `{}`: {e}", path.display())))
        }
        FileBacking::Mock { path, files } => files
            .get(path)
            .cloned()
            .ok_or_else(|| Error::msg(format!("read failed for mock Dir `{path}`: no such file"))),
    }
}

fn write_file_backing(file: &FileBacking, contents: String) -> Result<()> {
    match file {
        FileBacking::Fs(path) => {
            use std::io::Write;
            crate::confine::open_write(path)
                .and_then(|mut f| f.write_all(contents.as_bytes()))
                .map_err(|e| Error::msg(format!("write failed for `{}`: {e}", path.display())))
        }
        FileBacking::Mock { path, .. } => Err(Error::msg(format!(
            "write failed for mock Dir `{path}`: mock directories are read-only"
        ))),
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
    let DirBacking::Fs(base) = &dir.backing else {
        return Err(Error::msg("exec cannot run programs from an in-memory mock Dir"));
    };
    let prog = confine(crate::confine::resolve(base, &path))?;
    let argv: Vec<&str> = if joined.is_empty() { Vec::new() } else { joined.split('\0').collect() };
    use std::io::Write as _;
    use std::process::{Command, Stdio};
    let mut child = Command::new(&prog)
        .args(&argv)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| Error::msg(format!("exec failed to spawn `{}`: {e}", prog.display())))?;
    if let Some(mut sin) = child.stdin.take() {
        sin.write_all(stdin.as_bytes())
            .map_err(|e| Error::msg(format!("exec failed writing stdin to `{}`: {e}", prog.display())))?;
    }
    let output = child
        .wait_with_output()
        .map_err(|e| Error::msg(format!("exec failed running `{}`: {e}", prog.display())))?;
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
        DirBacking::Fs(base) => crate::confine::resolve(base, &rel)
            .map(|p| p.exists())
            .unwrap_or(false),
        DirBacking::Mock { root, files } => {
            mock_join(root, &rel).map(|path| mock_exists(files, &path)).unwrap_or(false)
        }
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
        DirBacking::Fs(base) => crate::confine::resolve(base, &rel)
            .map(|p| p.is_dir())
            .unwrap_or(false),
        DirBacking::Mock { root, files } => {
            mock_join(root, &rel).map(|path| mock_is_dir(files, &path)).unwrap_or(false)
        }
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
        DirBacking::Fs(base) => {
            let mut names: Vec<String> = std::fs::read_dir(base)
                .map_err(|e| Error::msg(format!("list failed for `{}`: {e}", base.display())))?
                .map(|entry| {
                    let entry =
                        entry.map_err(|e| Error::msg(format!("list failed for `{}`: {e}", base.display())))?;
                    entry.file_name().into_string().map_err(|_| {
                        Error::msg(format!(
                            "list failed for `{}`: directory entry name is not valid UTF-8",
                            base.display()
                        ))
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            names.sort();
            names
        }
        DirBacking::Mock { root, files } => mock_list(files, root)?,
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
        FileBacking::Fs(path) => {
            use std::io::Write as _;
            crate::confine::open_append(&path)
                .and_then(|mut f| f.write_all(contents.as_bytes()))
                .map_err(|e| Error::msg(format!("append failed for `{}`: {e}", path.display())))
        }
        FileBacking::Mock { path, .. } => Err(Error::msg(format!(
            "append failed for mock Dir `{path}`: mock directories are read-only"
        ))),
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
            let path = confine(crate::confine::resolve_write(base, &name))?;
            std::fs::create_dir_all(&path)
                .map_err(|e| Error::msg(format!("make_dir failed for `{}`: {e}", path.display())))
        }
        DirBacking::Mock { root, .. } => {
            let path = mock_join(root, &name)?;
            Err(Error::msg(format!(
                "make_dir failed for mock Dir `{path}`: mock directories are read-only"
            )))
        }
    }
}
