use wasmtime::{Caller, Error, Extern, ExternRef, Linker, Result, Rooted};
use witchy_test_host::{FixtureRoots, HostHandle, HostRequest, HostResponse};
use witchy_testkit::{SourceLocation, U64Text};

use super::super::{memory_of, read_wstr, read_wstr_list, slice, VmState};

pub(in crate::runtime) fn link_basic(
    linker: &mut Linker<VmState>,
    roots: &FixtureRoots,
) -> Result<()> {
    linker.func_wrap("witchy", "print_int", host_print_int)?;
    linker.func_wrap("witchy", "print_float", host_print_float)?;
    if roots.console {
        linker.func_wrap("witchy", "print", host_print)?;
        linker.func_wrap("witchy", "console_read_len", host_console_read_len)?;
    }
    if roots.clock {
        linker.func_wrap("witchy", "now", host_now)?;
        linker.func_wrap("witchy", "now_monotonic", host_now_monotonic)?;
    }
    if roots.rand {
        linker.func_wrap("witchy", "rand_u64", host_rand_u64)?;
    }
    if roots.env.is_some() {
        linker.func_wrap("witchy", "mint_env", host_mint_env)?;
        linker.func_wrap("witchy", "env_only", host_env_only)?;
        linker.func_wrap("witchy", "env_len", host_env_len)?;
        linker.func_wrap("witchy", "env_fill", host_env_fill)?;
    }
    Ok(())
}

pub(in crate::runtime) fn link_filesystem(
    linker: &mut Linker<VmState>,
    roots: &FixtureRoots,
) -> Result<()> {
    if roots.filesystem.is_none() {
        return Ok(());
    }
    linker.func_wrap("witchy", "mint_dir", host_mint_dir)?;
    linker.func_wrap("witchy", "dir_subdir", host_dir_subdir)?;
    linker.func_wrap("witchy", "dir_only", host_dir_only)?;
    linker.func_wrap("witchy", "dir_read_len", host_dir_read_len)?;
    linker.func_wrap("witchy", "dir_exists", host_dir_exists)?;
    linker.func_wrap("witchy", "dir_is_dir", host_dir_is_dir)?;
    linker.func_wrap("witchy", "dir_list_size", host_dir_list_size)?;
    linker.func_wrap("witchy", "dir_open", host_dir_open)?;
    linker.func_wrap("witchy", "dir_write", host_dir_write)?;
    linker.func_wrap("witchy", "dir_append", host_dir_append)?;
    linker.func_wrap("witchy", "dir_make_dir", host_dir_make_dir)?;
    linker.func_wrap("witchy", "dir_create", host_dir_create)?;
    linker.func_wrap("witchy", "file_read_len", host_file_read_len)?;
    linker.func_wrap("witchy", "file_write", host_file_write)?;
    Ok(())
}

pub(in crate::runtime) fn invoke(
    caller: &mut Caller<'_, VmState>,
    request: HostRequest,
) -> Result<HostResponse> {
    let source = fixture_source(caller);
    caller
        .data_mut()
        .fixture_host
        .as_mut()
        .ok_or_else(|| Error::msg("internal error: fixture import without a fixture host"))?
        .invoke(request, source)
        .map_err(|failure| {
            Error::msg(format!(
                "fixture {:?}: {}",
                failure.code, failure.message
            ))
        })
}

fn fixture_source(caller: &mut Caller<'_, VmState>) -> Option<SourceLocation> {
    let Extern::Global(global) = caller.get_export("__witchy_diagnostic_site")? else {
        return None;
    };
    let site = global.get(&mut *caller).i64()?;
    if site == 0 {
        return None;
    }
    let (function_ptr, line) = witchy_syntax::diag::unpack_site(site);
    let module = if function_ptr == 0 {
        String::new()
    } else {
        let memory = memory_of(caller).ok()?;
        let function = read_wstr(memory.data(&*caller), function_ptr as i32).ok()?;
        function
            .rsplit_once('.')
            .map_or(function.as_str(), |(module, _)| module)
            .to_owned()
    };
    Some(SourceLocation {
        module,
        line: U64Text::new(u64::from(line)),
        column: U64Text::new(1),
    })
}

fn fixture_handle(
    caller: &Caller<'_, VmState>,
    value: Option<Rooted<ExternRef>>,
    family: &str,
) -> Result<HostHandle> {
    value
        .ok_or_else(|| Error::msg(format!("{family} fixture externref is null")))?
        .data(caller)?
        .ok_or_else(|| Error::msg(format!("{family} fixture externref has no host data")))?
        .downcast_ref::<HostHandle>()
        .copied()
        .ok_or_else(|| Error::msg(format!("{family} fixture externref has wrong host data")))
}

fn root_handle(caller: &Caller<'_, VmState>, family: &str) -> Result<HostHandle> {
    let roots = caller
        .data()
        .fixture_host
        .as_ref()
        .ok_or_else(|| Error::msg("internal error: missing fixture host"))?
        .roots();
    match family {
        "Env" => roots.env,
        "Filesystem" => roots.filesystem,
        _ => None,
    }
    .ok_or_else(|| Error::msg(format!("fixture plan declared no {family} provider")))
}

fn host_print(mut caller: Caller<'_, VmState>, pointer: i32, length: i32) -> Result<()> {
    let memory = memory_of(&mut caller)?;
    let text = String::from_utf8_lossy(slice(memory.data(&caller), pointer, length)?)
        .trim_end_matches('\n')
        .to_owned();
    match invoke(
        &mut caller,
        HostRequest::ConsoleWrite { text: text.clone() },
    )? {
        HostResponse::Unit => {
            caller.data().output.lock().unwrap().push(text);
            Ok(())
        }
        response => Err(Error::msg(format!(
            "fixture Console write returned unexpected response {response:?}"
        ))),
    }
}

fn host_console_read_len(mut caller: Caller<'_, VmState>) -> Result<i32> {
    match invoke(&mut caller, HostRequest::ConsoleRead)? {
        HostResponse::String(line) => {
            let length = i32::try_from(line.len())
                .map_err(|_| Error::msg("Console fixture input exceeds the guest ABI size limit"))?;
            caller.data_mut().pending = Some(line.into_bytes());
            Ok(length)
        }
        response => Err(Error::msg(format!(
            "fixture Console read returned unexpected response {response:?}"
        ))),
    }
}

fn host_print_int(caller: Caller<'_, VmState>, value: i64) {
    caller
        .data()
        .output
        .lock()
        .unwrap()
        .push(value.to_string());
}

fn host_print_float(caller: Caller<'_, VmState>, value: f64) {
    caller
        .data()
        .output
        .lock()
        .unwrap()
        .push(witchy_syntax::fmt::render_float(value));
}

fn clock_value(caller: &mut Caller<'_, VmState>) -> Result<u64> {
    match invoke(caller, HostRequest::ClockNow)? {
        HostResponse::U64(value) => Ok(value),
        response => Err(Error::msg(format!(
            "fixture Clock returned unexpected response {response:?}"
        ))),
    }
}

fn host_now(mut caller: Caller<'_, VmState>) -> Result<i64> {
    Ok((clock_value(&mut caller)? / 1_000_000) as i64)
}

fn host_now_monotonic(mut caller: Caller<'_, VmState>) -> Result<i64> {
    Ok(clock_value(&mut caller)? as i64)
}

fn host_rand_u64(mut caller: Caller<'_, VmState>) -> Result<i64> {
    match invoke(&mut caller, HostRequest::RandU64)? {
        HostResponse::U64(value) => Ok(value as i64),
        response => Err(Error::msg(format!(
            "fixture Rand returned unexpected response {response:?}"
        ))),
    }
}

fn host_mint_env(mut caller: Caller<'_, VmState>) -> Result<Option<Rooted<ExternRef>>> {
    let handle = root_handle(&caller, "Env")?;
    ExternRef::new(&mut caller, handle).map(Some)
}

fn host_env_only(
    mut caller: Caller<'_, VmState>,
    env: Option<Rooted<ExternRef>>,
    names_pointer: i32,
) -> Result<Option<Rooted<ExternRef>>> {
    let env = fixture_handle(&caller, env, "Env")?;
    let memory = memory_of(&mut caller)?;
    let names = read_wstr_list(memory.data(&caller), names_pointer)?;
    match invoke(&mut caller, HostRequest::EnvOnly { env, names })? {
        HostResponse::Handle(handle) => ExternRef::new(&mut caller, handle).map(Some),
        response => Err(Error::msg(format!(
            "fixture Env.only returned unexpected response {response:?}"
        ))),
    }
}

fn host_env_len(
    mut caller: Caller<'_, VmState>,
    env: Option<Rooted<ExternRef>>,
    name_pointer: i32,
) -> Result<i32> {
    let env = fixture_handle(&caller, env, "Env")?;
    let memory = memory_of(&mut caller)?;
    let name = read_wstr(memory.data(&caller), name_pointer)?;
    match invoke(&mut caller, HostRequest::EnvGet { env, name })? {
        HostResponse::OptionalString(Some(value)) => {
            let length = i32::try_from(value.len())
                .map_err(|_| Error::msg("Env fixture value exceeds the guest ABI size limit"))?;
            caller.data_mut().pending = Some(value.into_bytes());
            Ok(length)
        }
        HostResponse::OptionalString(None) => {
            caller.data_mut().pending = None;
            Ok(-1)
        }
        response => Err(Error::msg(format!(
            "fixture Env.get returned unexpected response {response:?}"
        ))),
    }
}

fn host_env_fill(
    mut caller: Caller<'_, VmState>,
    _env: Option<Rooted<ExternRef>>,
    _name_pointer: i32,
    output_pointer: i32,
) -> Result<()> {
    let bytes = caller
        .data_mut()
        .pending
        .take()
        .ok_or_else(|| Error::msg("env_fill called without a staged fixture value"))?;
    let memory = memory_of(&mut caller)?;
    memory
        .write(&mut caller, output_pointer as usize, &bytes)
        .map_err(|error| Error::msg(format!("writing fixture Env value: {error}")))
}

fn host_mint_dir(
    mut caller: Caller<'_, VmState>,
    index: i32,
) -> Result<Option<Rooted<ExternRef>>> {
    if index != 0 {
        return Err(Error::msg(format!("invalid fixture Dir grant index {index}")));
    }
    let handle = root_handle(&caller, "Filesystem")?;
    ExternRef::new(&mut caller, handle).map(Some)
}

fn host_dir_subdir(
    mut caller: Caller<'_, VmState>,
    dir: Option<Rooted<ExternRef>>,
    name_pointer: i32,
) -> Result<Option<Rooted<ExternRef>>> {
    let dir = fixture_handle(&caller, dir, "Dir")?;
    let memory = memory_of(&mut caller)?;
    let name = read_wstr(memory.data(&caller), name_pointer)?;
    match invoke(&mut caller, HostRequest::DirSubdir { dir, name })? {
        HostResponse::Handle(handle) => ExternRef::new(&mut caller, handle).map(Some),
        response => Err(Error::msg(format!(
            "fixture Dir.subdir returned unexpected response {response:?}"
        ))),
    }
}

fn host_dir_only(
    mut caller: Caller<'_, VmState>,
    dir: Option<Rooted<ExternRef>>,
    policy_pointer: i32,
) -> Result<Option<Rooted<ExternRef>>> {
    let dir = fixture_handle(&caller, dir, "Dir")?;
    let memory = memory_of(&mut caller)?;
    let refine = read_wstr(memory.data(&caller), policy_pointer)?;
    match invoke(&mut caller, HostRequest::DirOnly { dir, refine })? {
        HostResponse::Handle(handle) => ExternRef::new(&mut caller, handle).map(Some),
        response => Err(Error::msg(format!(
            "fixture Dir.only returned unexpected response {response:?}"
        ))),
    }
}

fn host_dir_read_len(
    mut caller: Caller<'_, VmState>,
    dir: Option<Rooted<ExternRef>>,
    path_pointer: i32,
) -> Result<i32> {
    let dir = fixture_handle(&caller, dir, "Dir")?;
    let memory = memory_of(&mut caller)?;
    let path = read_wstr(memory.data(&caller), path_pointer)?;
    match invoke(&mut caller, HostRequest::DirRead { dir, path })? {
        HostResponse::Bytes(bytes) => stage_bytes(&mut caller, bytes, "Dir read"),
        response => Err(Error::msg(format!(
            "fixture Dir.read returned unexpected response {response:?}"
        ))),
    }
}

fn host_dir_exists(
    mut caller: Caller<'_, VmState>,
    dir: Option<Rooted<ExternRef>>,
    path_pointer: i32,
) -> Result<i32> {
    let dir = fixture_handle(&caller, dir, "Dir")?;
    let memory = memory_of(&mut caller)?;
    let path = read_wstr(memory.data(&caller), path_pointer)?;
    match invoke(&mut caller, HostRequest::DirExists { dir, path })? {
        HostResponse::Bool(value) => Ok(value.into()),
        response => Err(Error::msg(format!(
            "fixture Dir.exists returned unexpected response {response:?}"
        ))),
    }
}

fn host_dir_is_dir(
    mut caller: Caller<'_, VmState>,
    dir: Option<Rooted<ExternRef>>,
    path_pointer: i32,
) -> Result<i32> {
    let dir = fixture_handle(&caller, dir, "Dir")?;
    let memory = memory_of(&mut caller)?;
    let path = read_wstr(memory.data(&caller), path_pointer)?;
    match invoke(&mut caller, HostRequest::DirIsDir { dir, path })? {
        HostResponse::Bool(value) => Ok(value.into()),
        response => Err(Error::msg(format!(
            "fixture Dir.is_dir returned unexpected response {response:?}"
        ))),
    }
}

fn host_dir_list_size(
    mut caller: Caller<'_, VmState>,
    dir: Option<Rooted<ExternRef>>,
) -> Result<i32> {
    let dir = fixture_handle(&caller, dir, "Dir")?;
    match invoke(&mut caller, HostRequest::DirList { dir })? {
        HostResponse::Strings(names) => {
            let size = 4usize
                .checked_add(8usize.saturating_mul(names.len()))
                .and_then(|size| {
                    names
                        .iter()
                        .try_fold(size, |size, name| size.checked_add(4 + name.len()))
                })
                .ok_or_else(|| Error::msg("fixture directory listing exceeds ABI size limits"))?;
            let size = i32::try_from(size)
                .map_err(|_| Error::msg("fixture directory listing exceeds ABI size limits"))?;
            caller.data_mut().pending_list = Some(names);
            Ok(size)
        }
        response => Err(Error::msg(format!(
            "fixture Dir.list returned unexpected response {response:?}"
        ))),
    }
}

fn host_dir_open(
    mut caller: Caller<'_, VmState>,
    dir: Option<Rooted<ExternRef>>,
    path_pointer: i32,
) -> Result<Option<Rooted<ExternRef>>> {
    let dir = fixture_handle(&caller, dir, "Dir")?;
    let memory = memory_of(&mut caller)?;
    let path = read_wstr(memory.data(&caller), path_pointer)?;
    match invoke(&mut caller, HostRequest::DirOpen { dir, path })? {
        HostResponse::Handle(handle) => ExternRef::new(&mut caller, handle).map(Some),
        response => Err(Error::msg(format!(
            "fixture Dir.open returned unexpected response {response:?}"
        ))),
    }
}

fn host_dir_write(
    mut caller: Caller<'_, VmState>,
    dir: Option<Rooted<ExternRef>>,
    path_pointer: i32,
    contents_pointer: i32,
) -> Result<()> {
    let dir = fixture_handle(&caller, dir, "Dir")?;
    let memory = memory_of(&mut caller)?;
    let data = memory.data(&caller);
    let path = read_wstr(data, path_pointer)?;
    let bytes = read_wstr(data, contents_pointer)?.into_bytes();
    match invoke(&mut caller, HostRequest::DirWrite { dir, path, bytes })? {
        HostResponse::Count(_) => Ok(()),
        response => Err(Error::msg(format!(
            "fixture Dir.write returned unexpected response {response:?}"
        ))),
    }
}

fn host_dir_append(
    mut caller: Caller<'_, VmState>,
    dir: Option<Rooted<ExternRef>>,
    path_pointer: i32,
    contents_pointer: i32,
) -> Result<()> {
    let dir = fixture_handle(&caller, dir, "Dir")?;
    let memory = memory_of(&mut caller)?;
    let data = memory.data(&caller);
    let path = read_wstr(data, path_pointer)?;
    let bytes = read_wstr(data, contents_pointer)?.into_bytes();
    match invoke(&mut caller, HostRequest::DirAppend { dir, path, bytes })? {
        HostResponse::Count(_) => Ok(()),
        response => Err(Error::msg(format!(
            "fixture Dir.append returned unexpected response {response:?}"
        ))),
    }
}

fn host_dir_make_dir(
    mut caller: Caller<'_, VmState>,
    dir: Option<Rooted<ExternRef>>,
    path_pointer: i32,
) -> Result<()> {
    let dir = fixture_handle(&caller, dir, "Dir")?;
    let memory = memory_of(&mut caller)?;
    let path = read_wstr(memory.data(&caller), path_pointer)?;
    match invoke(&mut caller, HostRequest::DirMakeDir { dir, path })? {
        HostResponse::Unit => Ok(()),
        response => Err(Error::msg(format!(
            "fixture Dir.make_dir returned unexpected response {response:?}"
        ))),
    }
}

fn host_dir_create(
    mut caller: Caller<'_, VmState>,
    dir: Option<Rooted<ExternRef>>,
    path_pointer: i32,
) -> Result<Option<Rooted<ExternRef>>> {
    let dir = fixture_handle(&caller, dir, "Dir")?;
    let memory = memory_of(&mut caller)?;
    let path = read_wstr(memory.data(&caller), path_pointer)?;
    match invoke(&mut caller, HostRequest::DirCreate { dir, path })? {
        HostResponse::Handle(handle) => ExternRef::new(&mut caller, handle).map(Some),
        response => Err(Error::msg(format!(
            "fixture Dir.create returned unexpected response {response:?}"
        ))),
    }
}

fn host_file_read_len(
    mut caller: Caller<'_, VmState>,
    file: Option<Rooted<ExternRef>>,
) -> Result<i32> {
    let file = fixture_handle(&caller, file, "File")?;
    match invoke(&mut caller, HostRequest::FileRead { file })? {
        HostResponse::Bytes(bytes) => stage_bytes(&mut caller, bytes, "File read"),
        response => Err(Error::msg(format!(
            "fixture File.read returned unexpected response {response:?}"
        ))),
    }
}

fn host_file_write(
    mut caller: Caller<'_, VmState>,
    file: Option<Rooted<ExternRef>>,
    contents_pointer: i32,
) -> Result<()> {
    let file = fixture_handle(&caller, file, "File")?;
    let memory = memory_of(&mut caller)?;
    let bytes = read_wstr(memory.data(&caller), contents_pointer)?.into_bytes();
    match invoke(&mut caller, HostRequest::FileWrite { file, bytes })? {
        HostResponse::Count(_) => Ok(()),
        response => Err(Error::msg(format!(
            "fixture File.write returned unexpected response {response:?}"
        ))),
    }
}

fn stage_bytes(caller: &mut Caller<'_, VmState>, bytes: Vec<u8>, operation: &str) -> Result<i32> {
    let length = i32::try_from(bytes.len())
        .map_err(|_| Error::msg(format!("fixture {operation} exceeds the guest ABI size limit")))?;
    caller.data_mut().pending = Some(bytes);
    Ok(length)
}
