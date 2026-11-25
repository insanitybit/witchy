#!/usr/bin/env python3
"""Build the bounded checker and optional trusted ready binding; never mutate/install an SDK."""
import argparse, hashlib, json, os, re, shutil, signal, subprocess, sys, time
from pathlib import Path

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--compiler', required=True, type=Path)
parser.add_argument('--sdk', required=True, type=Path)
parser.add_argument('--out', required=True, type=Path)
parser.add_argument('--sysroot', type=Path)
parser.add_argument('--mbx', type=Path, help='absolute path to MBX, defaults to mbx on PATH')
parser.add_argument('--rustc', type=Path, help='absolute path to rustc, defaults to rustc on PATH')
parser.add_argument('--full-sdk', action='store_true', help='retain the complete input SDK as the body checker runtime')
parser.add_argument('--body-interface', action='store_true', help='build the V2 body-interface checker and trusted operator ready binding')
parser.add_argument('--launch-free-gib', type=int, default=16, help='admitted host launch floor, minimum 10 GiB')
args = parser.parse_args()
fixed_header = args.body_interface and sys.platform == 'darwin' and not args.full_sdk
if args.launch_free_gib < 10: parser.error('launch floor must be at least 10 GiB')
repo = Path(__file__).resolve().parents[1]
for path in [args.compiler, args.sdk, args.out] + ([args.sysroot] if args.sysroot else []):
    if not path.is_absolute(): parser.error('all paths must be absolute')
for name in ('mbx', 'rustc'):
    selected = getattr(args, name) or shutil.which(name)
    if not selected: parser.error(f'{name} is required')
    selected = Path(selected)
    if not selected.is_absolute(): parser.error(f'{name} path must be absolute')
    setattr(args, name, selected.resolve(strict=True))
compiler, sdk, out = args.compiler.resolve(strict=True), args.sdk.resolve(strict=True), args.out.resolve()
if out.is_relative_to(sdk) or out.is_relative_to(repo): parser.error('output must be outside SDK and source tree')
if shutil.disk_usage(out.parent).free < args.launch_free_gib * 2**30: parser.error(f'{args.launch_free_gib} GiB launch floor')
out.mkdir(mode=0o700)  # Fresh only: no overwrite, recovery, cleanup, or SDK preparation.
manifest = dict(schema='witchy.native-helper.build.diagnostic.v1', status='running', verified_immutable=False,
                sdk_root=str(sdk), commands=[], compiler=str(compiler), admission=False,
                helper_mode='BODY_INTERFACE_REUSE_DIAGNOSTIC' if args.body_interface else 'UNSEALED_DIAGNOSTIC')
cancel_signal = 0
def cancelled(signum, frame):
    global cancel_signal
    cancel_signal = signum
for signum in [signal.SIGINT, signal.SIGTERM]: signal.signal(signum, cancelled)
def check_cancel():
    if cancel_signal: raise KeyboardInterrupt('build cancelled: signal ' + str(cancel_signal))
env = dict(PATH='/usr/bin:/bin:/usr/sbin:/sbin', LEAN_PATH=str(sdk / 'lib/lean'), LEAN_CC='/usr/bin/clang')
def save():
    temporary = out / 'manifest.tmp'
    temporary.write_text(json.dumps(manifest, indent=2) + '\n')
    temporary.replace(out / 'manifest.json')
def digest(path):
    h, size = hashlib.sha256(), 0
    with path.open('rb') as stream:
        for block in iter(lambda: stream.read(65536), b''):
            size += len(block)
            if size > 2**30: raise RuntimeError('file pin exceeds 1 GiB')
            h.update(block)
    return h.hexdigest()
def bounded(path):
    with path.open('rb') as stream: data = stream.read(65537)
    if len(data) > 65536: raise RuntimeError('command output exceeds 64 KiB')
    return data

def run(argv, label, timeout=120):
    check_cancel()
    if shutil.disk_usage(out).free < args.launch_free_gib * 2**30: raise RuntimeError(f'{args.launch_free_gib} GiB launch floor')
    stdout, stderr = out / (label + '.stdout'), out / (label + '.stderr')
    row = dict(argv=list(map(str, argv)), label=label, environment=env.copy(), group_released=False)
    manifest['commands'].append(row); save()
    def attempt(action, kind):
        try: return action()
        except BaseException as error: row.setdefault(kind, []).append(repr(error))
    child, primary, start = None, None, time.monotonic()
    with stdout.open('xb') as output, stderr.open('xb') as errors:
        try:
            check_cancel(); row['spawn_outcome'] = 'unknown'; save()
            child = subprocess.Popen(row['argv'], cwd=out, env=env, stdin=subprocess.DEVNULL,
                                     stdout=output, stderr=errors, start_new_session=True)
            row.update(pid=child.pid, pgid=child.pid, spawn_outcome='owned'); save()
            check_cancel()
            while child.poll() is None:
                check_cancel()
                try: owned = os.getpgid(child.pid) == child.pid and os.getsid(child.pid) == child.pid
                except ProcessLookupError:
                    # macOS can remove process metadata before WNOHANG observes exit.
                    # Require our own child's exit status before treating this as completion.
                    try: child.wait(timeout=1)
                    except subprocess.TimeoutExpired: raise RuntimeError('child ownership unknown') from None
                    break
                if not owned: raise RuntimeError('child ownership unknown')
                if time.monotonic() - start > timeout: raise RuntimeError('command deadline')
                if shutil.disk_usage(out).free < 4 * 2**30: raise RuntimeError('4 GiB live disk floor')
                if max(stdout.stat().st_size, stderr.stat().st_size) > 65536: raise RuntimeError('command output bound')
                time.sleep(.02)
            if child.returncode != 0: raise RuntimeError('command failed: ' + label)
        except BaseException as error:
            primary = error; row['primary_error'] = repr(error)
            attempt(save, 'persistence_errors')
        finally:
            if child is not None:
                attempt(lambda: os.killpg(child.pid, signal.SIGKILL) if child.poll() is None else None, 'cleanup_errors')
                status = attempt(lambda: child.wait(timeout=5), 'cleanup_errors')
                row['seconds'] = time.monotonic() - start
                if status is not None: row['exit'] = status
                try: os.killpg(child.pid, 0)
                except ProcessLookupError: row['group_released'] = True
                except BaseException as error: row.setdefault('cleanup_errors', []).append(repr(error))
            attempt(save, 'persistence_errors')
    failures = {k: row[k] for k in ['primary_error', 'cleanup_errors', 'persistence_errors', 'pid'] if k in row}
    if row.get('cleanup_errors') or row.get('persistence_errors'):
        attempt(lambda: print(json.dumps(failures), file=sys.stderr), 'persistence_errors')
    if primary is not None: raise primary
    if row.get('cleanup_errors') or row.get('persistence_errors'): raise RuntimeError('command cleanup/persistence failed; claims retained')
    if shutil.disk_usage(out).free < 4 * 2**30: raise RuntimeError('4 GiB live disk floor')
    if not row['group_released']: raise RuntimeError('command group release unknown; reservation retained')
    result, errors = bounded(stdout), bounded(stderr)
    if row['exit'] != 0: raise RuntimeError('command failed: ' + label)
    return result.decode('utf-8')

def identity(label):
    value = json.loads(run([compiler, 'wasm-toolchain-id', sdk], label))
    if set(value) != {'algorithm', 'domain', 'hex'} or value['algorithm'] != 'sha256' or value['domain'] != 'witchy.scalar.lean-installation.v1':
        raise RuntimeError('unexpected SDK identity schema')
    if not isinstance(value['hex'], str) or not re.fullmatch('[0-9a-f]{64}', value['hex']): raise RuntimeError('invalid SDK digest')
    return value

def install_libraries(runtime):
    lib = runtime / 'lib/lean'
    lib.mkdir(parents=True, exist_ok=True)
    for file in (sdk / 'lib/lean').glob('*.dylib'):
        shutil.copyfile(file, lib / file.name)
    # Bundled libraries must resolve only adjacent @rpath libraries or system libraries.
    for file in lib.glob('*.dylib'):
        require_runtime_links(file, lib, False)
    return runtime


def require_runtime_links(file, lib, helper):
    paths = subprocess.check_output(['/usr/bin/otool','-L',file], text=True).splitlines()[1:]
    for row in paths:
        dep = row.strip().split(' (compatibility version')[0]
        if dep.startswith(('/usr/lib/','/System/Library/')): continue
        name = dep.removeprefix('@rpath/')
        if name == dep or '/' in name or not (lib / name).is_file():
            raise ValueError(f"unpackaged native dependency {dep}")
    commands = subprocess.check_output(['/usr/bin/otool','-l',file], text=True)
    rpaths = re.findall(r'cmd LC_RPATH\s+cmdsize \d+\s+path (.*?) \(offset', commands)
    if (helper and rpaths != [str(lib)]) or (not helper and any(r != '@loader_path' for r in rpaths)):
        raise ValueError(f"unexpected native search paths in {file}: {rpaths}")


try:
    helper_source = repo / ('tools/proof-session-body' if args.body_interface else 'tools/proof-session')
    sources = [Path(__file__).resolve(), helper_source / 'Resident.lean',
               repo / 'tools/proof-session/native.rs', repo / 'src/compiler/proof_prelude.lean.in']
    if args.body_interface:
        sources.append(repo / 'src/compiler/proof_audit.lean.in')
        shutil.copyfile(sources[-1], out / 'proof_audit.lean.in')
    pins = {str(p.relative_to(repo)): digest(p) for p in sources}
    compiler_pin = digest(compiler)
    manifest.update(source_sha256=pins, compiler_sha256=compiler_pin); save()
    sdk_id = identity('sdk-A'); manifest['sdk_identity_a'] = sdk_id; save()
    shutil.copyfile(sources[1], out / 'Resident.lean')
    shutil.copyfile(sources[2], out / 'native.rs')
    if args.sysroot:
        sysroot = args.sysroot.resolve(strict=True)
    else:
        sysroot = Path(run(['/usr/bin/xcrun', '--show-sdk-path'], 'system-headers', 10).strip())
    if not sysroot.is_absolute() or not sysroot.is_dir(): raise RuntimeError('system header root unavailable')
    deployment = run(['/usr/bin/sw_vers', '-productVersion'], 'host-deployment', 10).strip()
    if not re.fullmatch(r'\d+(\.\d+){1,2}', deployment): raise RuntimeError('host deployment version refused')
    env['MACOSX_DEPLOYMENT_TARGET'] = deployment
    run([sdk / 'bin/lean', '--trust=0', '--threads=1', '--memory=2048', '-c', out / 'Resident.c',
         '-o', out / 'Resident.olean', out / 'Resident.lean'], 'lean-C')
    declarations = (out / 'Resident.c').read_text()
    signatures = ['lean_object* witchy_resident_deadline(uint32_t);',
                  'LEAN_EXPORT lean_object* witchy_resident_main(lean_object*, lean_object*, lean_object*);']
    if args.body_interface:
        signatures.extend(['uint8_t witchy_use_fixed_header();',
                           'LEAN_EXPORT lean_object* witchy_save_header(lean_object*, lean_object*);'])
    if any(signature not in declarations for signature in signatures):
        raise RuntimeError('Lean generated an unsupported helper ABI')
    helper = out / 'proof-session-helper'
    runtime = install_libraries(out / 'runtime') if fixed_header else sdk
    rust_options = []
    if args.body_interface: rust_options.extend(['--cfg', 'witchy_body'])
    if fixed_header:
        import ctypes, uuid
        cache_uuid = (ctypes.c_ubyte * 16)()
        query = ctypes.CDLL('/usr/lib/libSystem.B.dylib')._dyld_get_shared_cache_uuid
        query.argtypes = [ctypes.POINTER(ctypes.c_ubyte)]
        query.restype = ctypes.c_bool
        if not query(cache_uuid): raise RuntimeError('Darwin shared cache identity unavailable')
        abi = str(uuid.UUID(bytes=bytes(cache_uuid)))
        env['WITCHY_FIXED_HEADER'] = abi
        manifest['os_shared_cache_uuid'] = abi
    # The Rust adapter uses the Lean 4.36 64-bit mimalloc object ABI.
    version = run([sdk / 'bin/lean', '--version'], 'lean-version', 10).strip()
    headers = {'lean.h': 'dbccdab3a0036f41494f22762adce633875e34cf8f6f7cfa2186175b0e964aa4',
               'config.h': 'e1abf0687bccdf7496ec031aa6a958aff29c8e7141ff32b62e90934fbe929873'}
    if (not version.startswith('Lean (version 4.36.0,') or
        any(digest(sdk / 'include/lean' / name) != expected for name, expected in headers.items())):
        raise RuntimeError('unsupported Lean runtime ABI')
    run([args.mbx, 'exec', '--project-root', repo, args.rustc, '--edition=2024', '--crate-type=lib',
         '--emit=obj', '-Copt-level=2', '-Cpanic=abort', *rust_options, out / 'native.rs', '-o', out / 'native.o'], 'native-rust')
    run([sdk / 'bin/leanc', '-O2', '-leanshared', '-Wl,-rpath,' + str(runtime / 'lib/lean'), '-L', sdk / 'lib',
         '-isysroot', sysroot, '-o', helper, out / 'Resident.c', out / 'native.o'], 'native-link')
    if fixed_header:
        run([helper, '--install-fixed-header', sdk, runtime / 'fixed-header.snap'], 'fixed-import-image')
    sdk_after = identity('sdk-B'); manifest['sdk_identity_b'] = sdk_after; save()
    if sdk_after != sdk_id: raise RuntimeError('SDK changed during helper build')
    if digest(compiler) != compiler_pin or {str(p.relative_to(repo)): digest(p) for p in sources} != pins:
        raise RuntimeError('compiler or helper source changed during build')
    check_cancel()
    if fixed_header:
        require_runtime_links(helper, runtime / 'lib/lean', True)
        (runtime / 'fixed-header.chunked-v1').touch(exist_ok=False)
        runtime_pin = json.loads(run([compiler, 'wasm-toolchain-id', runtime], 'runtime-identity'))
        if runtime_pin['domain'] != 'witchy.scalar.packed-imports.v1':
            raise RuntimeError('unexpected packed runtime identity domain')
        runtime_id = runtime_pin['hex']
        manifest.update(runtime=str(runtime), runtime_sha256=runtime_id)
    else:
        runtime_id = sdk_id['hex']
    if args.body_interface:
        # Operator-controlled build provenance, not a signature or an immutable capability.
        source_id = hashlib.sha256(sources[1].read_bytes() + b'\0' + sources[2].read_bytes() + b'\0' + sources[-1].read_bytes()).hexdigest()
        fields = ['witchy.ready-toolchain.v1', 'helper=' + str(helper), 'helper_sha256=' + digest(helper),
                  'sdk=' + str(runtime), 'sdk_sha256=' + runtime_id, 'helper_mode=body-interface-v2',
                  'source_sha256=' + source_id, 'trust_policy=selected_readonly_generation_no_concurrent_mutation']
        if fixed_header:
            fields[0] = 'witchy.ready-toolchain.v2'
            fields.append('darwin_shared_cache_uuid=' + abi)
        if any('\n' in field or '\r' in field for field in fields): raise RuntimeError('binding path contains newline')
        with (out / 'ready-binding.txt').open('x') as record: record.write('\n'.join(fields) + '\n')
        manifest['ready_binding'] = str(out / 'ready-binding.txt')
    manifest.update(status='built-unqualified', sdk_identity=sdk_id, helper_sha256=digest(helper),
                    generated_source_sha256=digest(out / 'Resident.lean'), system_headers=str(sysroot), deployment=deployment)
except BaseException as error:
    manifest.update(status='failed', error=repr(error)); raise
finally:
    primary = sys.exc_info()[1]
    try: save()
    except BaseException as secondary:
        manifest.setdefault('persistence_errors', []).append(repr(secondary))
        try: print('manifest persistence failure: ' + repr(secondary) + '; primary: ' + repr(primary), file=sys.stderr)
        except BaseException as error: manifest['persistence_errors'].append(repr(error))
        if primary is None: raise
