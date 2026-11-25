"""Run an installed Grimoire against a local Coven HTTPS deployment.

Use the approval fixture environment and matching compiler, proof binding and host.
This witness creates its own signing keys, trust anchors, tool install and state.
"""
import argparse
import base64
from concurrent.futures import ThreadPoolExecutor
from contextlib import ExitStack
from datetime import datetime, timedelta, timezone
import hashlib
import http.client
import json
import os
from pathlib import Path
import shutil
import socket
import ssl
import subprocess
import sys
import tempfile
import time
import tomllib
from urllib.parse import urlsplit
from zipfile import ZipFile

from cryptography import x509
from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import ec, mldsa, rsa
from securesystemslib.signer import CryptoSigner
from tuf.api.metadata import DelegatedRole, Delegations, Metadata, Snapshot, TargetFile, Targets, Timestamp

CHECKOUT = Path(__file__).resolve().parents[3]
COVEN = CHECKOUT / "coven/witchy2"
GRANT_DOCUMENT = b"# Output is authorized in the project manifest.\n"
LONG_PUBLIC_NAME = ".".join(("a" * 63, "b" * 63, "c" * 63, "d" * 56, "test"))
sys.path.insert(0, str(COVEN))

from check_deployed_tuf_run import check as check_deployed_run
from check_tuf_cancellation import fixture_tls_context, verify_interrupted_tuf

import coven_approval as approval
from coven_ci_http import CHALLENGE, TRANSPORT_BUILD, post
from coven_https_client import CA_FILE_BYTES, downloader
from coven_review_commit import commit_worker, issuer_worker
from coven_tls_prepare import BUILD as TLS_PREPARE_BUILD
import coven_staging as staging
from coven_target import FORMAT, seal, seal_snapshot_worker, sign_package, signer_worker
from test_coven_approval import ReleaseApproval
from test_coven_tuf_publish import COMPILER, HOST, MirrorPublication
from witchy_release import digest_of, pack


def witness(lifetime, *, web=False, chromium=None, client_lean_root=None, client_python=None,
            tuf_workers=None, manifest_worker=None, platform_ca_file=None, export_directory=None, fixture_port=18443,
            fixture_default_ca=False):
    if not COMPILER or not HOST:
        raise RuntimeError("set WITCHYC and WITCHY_WASMTIME_HOST to the matching tools")
    work = Path(lifetime.enter_context(tempfile.TemporaryDirectory(prefix="coven-https-witness-"))).resolve()
    mirror = MirrorPublication()
    lifetime.callback(mirror.doCleanups)
    mirror.setUpClass()
    mirror.setUp()
    release = ReleaseApproval()
    lifetime.callback(release.doCleanups)
    release.setUp()
    build = mirror.build
    release.commit = lifetime.enter_context(commit_worker(build, HOST))
    release.issue = lifetime.enter_context(issuer_worker(build, HOST))
    publisher_sign = lifetime.enter_context(signer_worker(build, HOST, "publisher"))
    witness_sign = lifetime.enter_context(signer_worker(build, HOST, "witness"))
    witness_snapshot = lifetime.enter_context(seal_snapshot_worker(build, HOST))

    def seed(name):
        path = release.root / name
        private = os.urandom(32)
        with os.fdopen(os.open(path, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600), "wb") as output:
            output.write(private)
        return path, approval.b64(mldsa.MLDSA87PrivateKey.from_seed_bytes(private).public_key().public_bytes_raw())

    witness_key, witness_public = seed("witness")
    names = ["helper", "lib"] + (["glamour"] if web else [])
    release.policy["credentials"][release.id]["packages"] = sorted(names)
    keys = {name: seed(name) for name in names}
    trust = staging.canonical({"format": FORMAT, "policy": release.policy, "witness": witness_public,
                               "publishers": {name: key[1] for name, key in keys.items()}})
    mirror.args.trust.write_bytes(trust)
    mirror.args.trust_pin = digest_of(trust)
    with staging.transaction(release.stage) as db:
        db.execute("UPDATE policy SET document=?", (staging.canonical(release.policy),))
        db.execute("DELETE FROM candidates")
    helper_files = {
        "helper.wy": b'import "number.wy"\n\npub fn base() -> U64:\n    return base_number()\n',
        # Make the signed helper target span multiple mirror body chunks.
        "number.wy": b"pub fn base_number() -> U64:\n    return 41\n" + b"// mirror framing fixture\n" * 1400,
    }
    lib_files = {
        "lib.wy": b'''import "@helper"

fn first(values: &[U64]) -> U64:
    requires values.len() > 0
    ensures true
    proof:
        auto
    return values[0]

pub fn one() -> U64:
    let values: [U64; 1] = [base() + 1]
    let slice = values.slice()
    return first(slice)
''',
    }
    sources = {
        "helper": pack("helper", "1.0.0", {}, "helper.wy", helper_files),
        "lib": pack("lib", "1.0.0", {"helper": "1.0.0"}, "lib.wy", lib_files),
    }
    if web:
        directory = CHECKOUT / "grimoire/examples/glamour-catalog/glamour"
        sources["glamour"] = pack("glamour", "0.3.0", {}, "glamour.wy",
                                  {path.name: path.read_bytes() for path in directory.glob("*.wy")})
    roles, delegations, public = {}, {}, {}
    for counter, (name, payload) in enumerate(sources.items(), 1):
        version = "0.3.0" if name == "glamour" else "1.0.0"
        if name not in mirror.signers:
            mirror.signers[name] = CryptoSigner.generate_mldsa(scheme="ml-dsa-87/1")
        digest = digest_of(payload)
        with staging.transaction(release.stage) as db:
            db.execute("INSERT INTO candidates VALUES (?, ?, ?, ?)", (digest, name, version, payload))
        token = approval.challenge(release.stage, digest, release.issue, release.commit)["publicKey"]["challenge"]
        release.approve(token, release.assertion(token, counter=counter))
        package, signature, sealed = (release.root / f"{name}.{ext}" for ext in ("package", "signature", "sealed"))
        package.write_bytes(payload)
        sign_package(package, keys[name][0], "test", signature, publisher_sign)
        seal(release.stage, digest, signature, mirror.args.trust, witness_key, sealed, witness_sign, witness_snapshot)
        wire = sealed.read_bytes()
        directory = mirror.args.incoming / "targets" / name
        directory.mkdir(parents=True)
        (directory / f"{digest_of(wire)[7:]}.{version}.coven").write_bytes(wire)
        coordinate = f"{name}/{version}.coven"
        target = TargetFile.from_data(coordinate, wire, ["sha384"])
        delegations[f"{name}.json"] = mirror.issue(name, Targets(expires=mirror.expiry, targets={coordinate: target}), f"1.{name}.json")
        key = mirror.signers[name].public_key
        public[key.keyid] = key
        roles[name] = DelegatedRole(name, [key.keyid], 1, True, paths=[f"{name}/*"])
    delegations["targets.json"] = mirror.issue("targets", Targets(expires=mirror.expiry, delegations=Delegations(public, roles)), "1.targets.json")

    snapshots = {}

    def publish(version):
        reference = mirror.issue("snapshot", Snapshot(version=version, expires=mirror.expiry, meta=delegations), f"{version}.snapshot.json")
        snapshots[str(version)] = "sha384:" + reference.hashes["sha384"]
        mirror.issue("timestamp", Timestamp(version=version + 3, expires=mirror.expiry, snapshot_meta=reference), "timestamp.json")
        print("PUBLISHED", mirror.publish(), flush=True)
        return reference

    publish(1)
    initial_timestamp_pin = digest_of((mirror.metadata / "timestamp.json").read_bytes())
    old_timestamp = (mirror.mirror / "metadata/timestamp.json").read_bytes()

    if export_directory is None:
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            port = listener.getsockname()[1]
    else:
        port = fixture_port
    server = ec.generate_private_key(ec.SECP384R1())
    leaf_cert, intermediate_cert, root_cert, unrelated_cert = fixture_chain(server)
    chain = (leaf_cert, intermediate_cert)
    tls = mirror.mirror / "tls"
    tls.mkdir(mode=0o755)
    (tls / "authority").write_text(f"localhost:{port}\n")
    with (tls / "chain").open("wb") as output:
        for certificate in chain:
            der = certificate.public_bytes(serialization.Encoding.DER)
            output.write(len(der).to_bytes(3, "big") + der)
    tls_key = release.root / "server.der"
    tls_key.write_bytes(server.private_bytes(serialization.Encoding.DER, serialization.PrivateFormat.PKCS8, serialization.NoEncryption()))
    tls_key.chmod(0o600)

    project = release.root / "project"
    project.mkdir()
    (project / "local").mkdir()
    (project / "local/value.wy").write_text("pub fn checked_value(value: U64) -> U64:\n    return value\n")
    (project / "main.wy").write_text('import "@lib"\nimport "local/value.wy"\n\n'
                                    'fn main(output: Console<Write>):\n    output.print_int(checked_value(one()))\n')
    (project / "main.grants.toml").write_bytes(GRANT_DOCUMENT)
    shutil.copyfile(mirror.args.root, project / "root.json")
    shutil.copyfile(mirror.args.trust, project / "trust.json")
    ca_file = project / "ca.pem"
    wrong_ca = unrelated_cert.public_bytes(serialization.Encoding.PEM)
    fixture_roots = wrong_ca + root_cert.public_bytes(serialization.Encoding.PEM)
    if platform_ca_file is not None:
        platform_roots = staging.read_file(platform_ca_file.resolve(strict=True), CA_FILE_BYTES - len(fixture_roots) - 1)
        fixture_roots += platform_roots + b"\n"
        print(f"using {len(fixture_roots)} bytes of fixture and platform CA roots", flush=True)
    ca_file.write_bytes(fixture_roots)
    ca_pin = digest_of(fixture_roots)
    origin = "https://localhost" if port == 443 else f"https://localhost:{port}"
    ca_file_setting = "" if fixture_default_ca else 'ca-file = "ca.pem"\n'
    (project / "witchy.toml").write_text(f'''[run]
entry = "main.wy"

[grants]
document = "main.grants.toml"
output = true

[registry]
protocol = "tuf"
url = "{origin}"
id = "test"
root = "root.json"
root-pin = "{mirror.args.root_pin}"
trust = "trust.json"
trust-pin = "{mirror.args.trust_pin}"
{ca_file_setting}ca-pin = "{ca_pin}"
[dependencies]
lib = "1.0.0"

[[test]]
name = "installed registry project"
project = "."
stdout = "42\\n"
''')

    manifest_pin = digest_of((project / "witchy.toml").read_bytes())
    packages = {}
    for name, files, dependencies in (("helper", helper_files, []), ("lib", lib_files, ["helper 1.0.0"])):
        packages[name] = {
            "digest": digest_of(sources[name]),
            "target": digest_of((release.root / f"{name}.sealed").read_bytes()),
            "source": digest_of(files[f"{name}.wy"]),
            "publisher": digest_of(base64.urlsafe_b64decode(keys[name][1] + "===")),
            "requires": dependencies,
            "version": "1.0.0",
        }
        if len(files) > 1:
            packages[name]["files"] = {path: digest_of(wire) for path, wire in files.items()}
    inventory = {
        "stdout": "42\n", "packages": packages, "snapshots": snapshots,
        "registry": {"url": origin, "id": "test", "root-pin": mirror.args.root_pin,
                     "trust-pin": mirror.args.trust_pin, "ca-pin": ca_pin},
        "manifest": manifest_pin, "grants": digest_of(GRANT_DOCUMENT),
        "sources": {name: digest_of((project / name).read_bytes()) for name in ("main.wy", "local/value.wy")},
    }
    if export_directory is not None:
        export_fixture(export_directory, project, tls_key, mirror.mirror, publish, inventory)
        return

    installer = CHECKOUT / "grimoire/install.py"
    bundle = work / "client.zip"
    prebuilt = ["--tuf-workers", tuf_workers.resolve(strict=True)] if tuf_workers is not None else []
    if manifest_worker is not None:
        prebuilt.extend(("--manifest-worker", manifest_worker.resolve(strict=True)))
    subprocess.run([sys.executable, installer, "--make-bundle", bundle,
                    "--compiler-binary", COMPILER, "--host-binary", HOST, "--coven", CHECKOUT / "coven", *prebuilt],
                   check=True, capture_output=True, timeout=30)
    pin = digest_of(bundle.read_bytes())
    with ZipFile(bundle) as archive:
        toolchain = {"compiler": digest_of(archive.read("bin/witchyc")),
                     "host": digest_of(archive.read("bin/witchy-wasmtime-host")),
                     "host-libraries": {
                         Path(name).name: digest_of(archive.read(name))
                         for name in archive.namelist() if name.startswith("bin/libaws_lc_fips_")
                     }}
    client_home = release.root / "client-home"
    client_home.mkdir(mode=0o700)
    detached = client_home / "install.py"
    shutil.copyfile(installer, detached)
    proof_cache = client_home / "proof-cache"
    proof_cache.mkdir(mode=0o700)
    env = {key: value for key, value in os.environ.items() if key in
           ("WITCHY_WASM_READY_BINDING", "WITCHY_WASM_LEAN_ROOT", "TMPDIR")}
    if client_lean_root is not None:
        env.pop("WITCHY_WASM_READY_BINDING", None)
        env["WITCHY_WASM_LEAN_ROOT"] = str(client_lean_root.resolve(strict=True))
    env["HOME"] = str(client_home)
    # Supply trust as the detached launcher's default PEM file.
    env["SSL_CERT_FILE"] = str(ca_file)
    env["WITCHY_PROOF_CACHE"] = str(proof_cache.resolve(strict=True))
    python = Path(sys.executable) if client_python is None else client_python / "python3"
    env["PATH"] = os.pathsep.join(map(str, (python.parent, "/usr/bin", "/bin")))
    env["XDG_STATE_HOME"] = str(project / "state")
    env["XDG_CACHE_HOME"] = str(project / "cache")
    prefix = client_home / "installed"
    installed = subprocess.run([python, detached, "--from-bundle", bundle, "--bundle-pin", pin, "--prefix", prefix],
                               cwd=client_home, env=env, capture_output=True, text=True, timeout=60)
    assert installed.returncode == 0, installed.stderr
    verify = [python, detached, "--verify-bundle", bundle, "--bundle-pin", pin, "--prefix", prefix]
    subprocess.run(verify, cwd=client_home, env=env, check=True, capture_output=True, timeout=30)
    if tuf_workers is not None:
        worker = prefix / "libexec/coven/tuf-workers/tuf_get.wasm"
        original = worker.read_bytes()
        try:
            worker.write_bytes(original[:-1] + bytes([original[-1] ^ 1]))
            rejected = subprocess.run(verify, cwd=client_home, env=env, capture_output=True, timeout=30)
            assert rejected.returncode == 1 and b"tuf_get.wasm: differs from the pinned client bundle" in rejected.stderr
        finally:
            worker.write_bytes(original)
        subprocess.run(verify, cwd=client_home, env=env, check=True, capture_output=True, timeout=30)
    extra = prefix / "libexec/coven/unlisted.py"
    extra.write_bytes(b"# unlisted fixture\n")
    try:
        rejected = subprocess.run(verify, cwd=client_home, env=env, capture_output=True, timeout=30)
        assert rejected.returncode == 1 and b"unlisted.py: not in the pinned client bundle" in rejected.stderr
    finally:
        extra.unlink()
    binary = prefix / "bin"
    executable = binary / "coven"
    env["PATH"] = str(binary) + os.pathsep + env["PATH"]
    print("installed and checked the pinned client from a standalone installer", flush=True)
    module = work / "mirror-https.wasm"
    subprocess.run([COMPILER, "wasm-caps", "--policy", COVEN / "mirror_https.policy",
                    COVEN / "mirror_https.wy"], check=True, stdout=subprocess.DEVNULL)
    build("mirror_https", module, ("--heap-pages=9", "--slots=64", "--stack-bytes=16384"))
    log = lifetime.enter_context((work / "mirror.log").open("w+"))
    fullchain = work / "fullchain.pem"
    fullchain.write_bytes(b"".join(cert.public_bytes(serialization.Encoding.PEM) for cert in chain))
    checked_chain = work / "checked-chain"
    prepare_module = work / "tls-prepare.wasm"
    subprocess.run([COMPILER, "wasm-caps", "--policy", COVEN / "tls_prepare.policy",
                    COVEN / "tls_prepare.wy"], check=True, stdout=subprocess.DEVNULL)
    build("tls_prepare", prepare_module, TLS_PREPARE_BUILD)
    verify_identity = [executable, "tls-prepare", "--ca-file", ca_file]
    prepare_pin = digest_of(prepare_module.read_bytes())
    checked_worker = ["--module", prepare_module, "--module-pin", prepare_pin]
    subprocess.run([*verify_identity, *checked_worker, "--key", tls_key, "--fullchain", fullchain,
                    "--origin", origin, "--out", checked_chain], env=env, check=True, capture_output=True, timeout=10)
    assert checked_chain.read_bytes() == (tls / "chain").read_bytes()

    refused_chain = work / "refused-chain"
    missing_intermediate = work / "leaf.pem"
    missing_intermediate.write_bytes(leaf_cert.public_bytes(serialization.Encoding.PEM))
    wrong_key = work / "wrong-server.der"
    wrong_key.write_bytes(ec.generate_private_key(ec.SECP384R1()).private_bytes(
        serialization.Encoding.DER, serialization.PrivateFormat.PKCS8, serialization.NoEncryption()))
    wrong_key.chmod(0o600)
    for certificates, claimed_origin, key in ((fullchain, f"https://wrong.test:{port}", tls_key),
                                               (missing_intermediate, origin, tls_key),
                                               (fullchain, origin, wrong_key)):
        refused = subprocess.run([*verify_identity, *checked_worker, "--key", key, "--fullchain", certificates,
                                  "--origin", claimed_origin, "--out", refused_chain],
                                 env=env, capture_output=True, timeout=10)
        assert refused.returncode and b"Witchy refused the TLS certificate chain" in refused.stderr, refused.stderr
        assert not refused_chain.exists()

    changed_prepare = work / "changed-prepare.wasm"
    changed_prepare.write_bytes(prepare_module.read_bytes() + b"changed")
    refused = subprocess.run([*verify_identity, "--module", changed_prepare, "--module-pin", prepare_pin,
                              "--key", tls_key, "--fullchain", fullchain, "--origin", origin,
                              "--out", refused_chain], env=env, capture_output=True, timeout=10)
    assert refused.returncode and b"TLS identity module differs from its independent pin" in refused.stderr, refused.stderr
    assert not refused_chain.exists()
    print("prepared the exact chain and refused a missing intermediate, wrong name, wrong key or changed module", flush=True)
    tls_check_module = work / "mirror-tls-check.wasm"
    subprocess.run([COMPILER, "wasm-caps", "--policy", COVEN / "mirror_tls_check.policy",
                    COVEN / "mirror_tls_check.wy"], check=True, stdout=subprocess.DEVNULL)
    build("mirror_tls_check", tls_check_module, TLS_PREPARE_BUILD)
    base = [executable, "tuf", "serve", "--mirror", mirror.mirror,
            "--listen", f"127.0.0.1:{port}", "--module", module, "--tls-key", tls_key]
    check_pin = digest_of(tls_check_module.read_bytes())
    launch = [*base, "--tls-ca-file", ca_file, "--tls-ca-pin", ca_pin,
              "--tls-check-module", tls_check_module, "--tls-check-pin", check_pin]
    notify_root = lifetime.enter_context(tempfile.TemporaryDirectory(prefix="coven-notify-", dir="/tmp"))
    destination = str(Path(notify_root) / "ready")
    ready = lifetime.enter_context(socket.socket(socket.AF_UNIX, socket.SOCK_DGRAM))
    ready.bind(destination)
    server_env = env | {"NOTIFY_SOCKET": destination}
    module_pin = digest_of(module.read_bytes())

    def refuse_start(pin, message, command=launch):
        result = subprocess.run([*command, "--module-pin", pin], env=server_env,
                                capture_output=True, timeout=10)
        assert result.returncode != 0 and message in result.stderr, result.stderr
        ready.setblocking(False)
        try:
            raise AssertionError(f"refused launch sent readiness: {ready.recv(64)!r}")
        except BlockingIOError:
            pass

    refuse_start(module_pin, b"mirror readiness requires", base)
    refuse_start(module_pin, b"mirror TLS check module differs from its independent pin",
                 launch[:-1] + ["sha384:" + "0" * 96])
    refuse_start(module_pin, b"Witchy refused the installed mirror TLS certificate chain",
                 [wrong_key if item == tls_key else item for item in launch])
    wrong_roots = work / "wrong-ca.pem"
    wrong_roots.write_bytes(wrong_ca)
    refuse_start(module_pin, b"Witchy refused the installed mirror TLS certificate chain",
                 [wrong_roots if item == ca_file else item for item in launch])
    equivalent_roots = work / "equivalent-ca.pem"
    equivalent_roots.write_bytes(fixture_roots + b"\n")
    refuse_start(module_pin, b"mirror TLS CA bundle differs from its independent pin",
                 [equivalent_roots if item == ca_file else item for item in launch])
    refuse_start(module_pin, b"--tls-ca-pin requires an independent SHA-384 pin",
                 ["" if item == ca_pin else item for item in launch])
    authority = tls / "authority"
    installed_name = authority.read_bytes()
    try:
        authority.write_text(f"wrong.test:{port}\n")
        refuse_start(module_pin, b"Witchy refused the installed mirror TLS certificate chain")
    finally:
        authority.write_bytes(installed_name)
    refuse_start("sha384:" + "0" * 96, b"module differs from its independent pin")
    with socket.socket() as unrelated:
        unrelated.bind(("127.0.0.1", port))
        unrelated.listen()
        refuse_start(module_pin, b"listening address unavailable")

    server_process = subprocess.Popen([*launch, "--module-pin", module_pin], stdout=log, stderr=log, env=server_env)
    def stop():
        if server_process.poll() is None:
            server_process.terminate()
        try:
            server_process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            server_process.kill()
            server_process.wait()
            raise
    lifetime.callback(stop)
    ready.settimeout(10)
    try:
        assert ready.recv(64) == b"READY=1"
    except TimeoutError:
        log.seek(0)
        raise RuntimeError(f"mirror startup failed: {log.read()}") from None
    print("verified TLS identity before readiness and refused wrong keys, roots, names, module pins and occupied bind", flush=True)

    def grimoire(*args, succeeds=True, state=None, directory=project):
        local_env = env if state is None else env | {"XDG_STATE_HOME": str(project / state)}
        command = (binary / "grimoire", *args, directory)
        start = time.perf_counter()
        result = subprocess.run(command, capture_output=True, text=True, env=local_env, timeout=90)
        print("grimoire", *args, f"returned {result.returncode} in {time.perf_counter()-start:.3f}s", flush=True)
        assert (result.returncode == 0) == succeeds, result.stdout + result.stderr
        return result

    def check_publication(snapshot, timestamp, *, default_ca=False, refused=None):
        command = [executable, "tuf", "catalog", "--registry", origin, "--id", "test",
                   "--root", project / "root.json", "--root-pin", mirror.args.root_pin,
                   "--trust", project / "trust.json", "--trust-pin", mirror.args.trust_pin,
                   "--ca-pin", ca_pin,
                   "--expect-snapshot", snapshot, "--expect-timestamp", timestamp]
        if not default_ca:
            command += ["--ca-file", ca_file]
        check_env = env | {"XDG_STATE_HOME": str(work / "publication-state")}
        result = subprocess.run(command, env=check_env, capture_output=True, text=True, timeout=40)
        if refused is not None:
            assert result.returncode == 1 and not result.stdout and refused in result.stderr, result
        else:
            assert result.returncode == 0, result.stderr
            verified = json.loads(result.stdout)
            assert (verified["snapshot"], verified["timestamp"], verified["ca_pin"]) == (snapshot, timestamp, ca_pin)

    check_publication(snapshots["1"], initial_timestamp_pin, default_ca=True)
    check_publication(digest_of(b"another release snapshot"), initial_timestamp_pin,
                      refused="verified TUF snapshot differs from the expected digest")
    print("authenticated the deployed publication and refused an unexpected signed snapshot", flush=True)
    probe_module = work / "mirror-check.wasm"
    subprocess.run([COMPILER, "wasm-caps", "--policy", COVEN / "mirror_check.policy",
                    COVEN / "mirror_check.wy"], check=True, stdout=subprocess.DEVNULL)
    build("mirror_check", probe_module, ("--heap-pages=48", "--slots=4096", "--stack-bytes=65536"))
    probe_pin = digest_of(probe_module.read_bytes())
    probe_env = env | {"PATH": os.pathsep.join(map(str, (python.parent, "/usr/bin", "/bin"))),
                       "WITCHY_WASMTIME_HOST": str(binary / "witchy-wasmtime-host"),
                       "WITCHYC": "/no-compiler-needed"}

    def probe(route, *, expected=None, refusal=None, roots_pin=ca_pin, module_pin=probe_pin):
        command = (executable, "mirror-check", "--origin", origin, "--route", route, "--ca-file", ca_file,
                   "--ca-pin", roots_pin, "--module", probe_module, "--module-pin", module_pin)
        result = subprocess.run(command, env=probe_env, capture_output=True, timeout=10)
        if refusal is None:
            assert result.returncode == 0 and json.loads(result.stdout) == {"content_length": expected, "status": 200}, result
        else:
            assert result.returncode == 1 and not result.stdout and refusal in result.stderr, result

    probe("timestamp", expected=(mirror.mirror / "metadata/timestamp.json").stat().st_size)
    probe("timestamp", roots_pin=digest_of(b"wrong anchors"), refusal=b"HTTPS CA bundle does not match its pin")
    probe("timestamp", module_pin=digest_of(b"wrong module"), refusal=b"module differs from its independent pin")
    probe("catalog", refusal=b"Witchy refused the mirror identity or route response")
    print("compiler-free mirror check verified timestamp headers and refused wrong pins and an absent catalog", flush=True)
    verify_bodyless_routes(project, port)
    verify_public_authority(project, port, mirror.mirror)
    verify_tuf_responses(project, port, mirror.mirror)
    verify_oversized_objects(project, port, mirror.mirror)
    timestamp_wire = (mirror.mirror / "metadata/timestamp.json").read_bytes()
    with downloader(build, HOST, urlsplit(f"https://localhost:{port}"), project / "ca.pem") as transport:
        destination = work / "bounded-timestamp"
        transport.fetch("/metadata/timestamp.json", destination, max_length=len(timestamp_wire))
        assert destination.read_bytes() == timestamp_wire
        too_small = work / "over-limit-timestamp"
        try:
            transport.fetch("/metadata/timestamp.json", too_small, max_length=len(timestamp_wire) - 1)
        except ValueError as error:
            assert str(error) == "Witchy TUF download refused"
        else:
            raise AssertionError("HTTPS download exceeded its requested maximum")
        assert not too_small.exists()
    for expected_ca_pin in (None, ca_pin):
        with socket.create_server(("127.0.0.1", 0)) as listener:
            unopened = urlsplit(f"https://localhost:{listener.getsockname()[1]}")
            with downloader(build, HOST, unopened, ca_file, ca_pin=expected_ca_pin) as transport:
                assert transport.ca_pin == ca_pin
                # Change the worker's staged file after the launcher has captured it.
                (transport.root / "roots.pem").write_bytes(fixture_roots[len(wrong_ca):] + wrong_ca)
                destination = work / "unexpected-timestamp"
                try:
                    transport.fetch("/metadata/timestamp.json", destination)
                except ValueError as error:
                    assert str(error) == "HTTPS CA bundle does not match its pin"
                else:
                    raise AssertionError("Witchy accepted an altered staged CA bundle")
                assert not destination.exists()
                listener.setblocking(False)
                try:
                    connection, _ = listener.accept()
                except BlockingIOError:
                    pass
                else:
                    connection.close()
                    raise AssertionError("Witchy opened a connection before checking the CA pin")
    print("the Witchy worker refused altered staged PEM bytes with and without a configured CA pin", flush=True)
    verify_abandoned_download(build, work, project, fullchain, tls_key)
    verify_ci_transport(build, work, project, fullchain, tls_key)
    if tuf_workers is not None:
        output = work / "bundled-release"
        command = [executable, "tuf", "fetch", "lib", "1.0.0", "--out", output,
                   "--registry", f"https://localhost:{port}", "--id", "test",
                   "--root", project / "root.json", "--root-pin", mirror.args.root_pin,
                   "--trust", project / "trust.json", "--trust-pin", mirror.args.trust_pin]
        fetch_env = env | {"WITCHYC": "/no/compiler", "XDG_STATE_HOME": str(work / "bundled-state")}
        refused = subprocess.run(command, env=fetch_env | {"SSL_CERT_FILE": str(wrong_roots)},
                                 capture_output=True, timeout=30)
        assert refused.returncode != 0 and not output.exists(), refused
        assert b"Witchy TUF download refused" in refused.stderr, refused.stderr
        refused = subprocess.run([*command, "--ca-pin", ca_pin],
                                 env=fetch_env | {"SSL_CERT_FILE": str(wrong_roots)},
                                 capture_output=True, timeout=30)
        assert refused.returncode == 1 and not output.exists(), refused
        assert b"HTTPS CA bundle does not match its pin" in refused.stderr, refused.stderr
        fetched = subprocess.run(command, env=fetch_env, capture_output=True, timeout=30)
        assert fetched.returncode == 0, fetched.stderr
        assert json.loads(fetched.stdout)["ca_pin"] == ca_pin
        assert output.read_bytes() == sources["lib"]
        print("verified a compiler-free fetch using the bundled TUF workers and default PEM trust path", flush=True)
        output.unlink()
        context = fixture_tls_context(fullchain, tls_key)
        verify_interrupted_tuf(binary / "grimoire", env, work, project, context)
        verify_interrupted_tuf(binary / "grimoire", env, work, project, context, deadline_ms=5000)
        roles = {"selector": ("tuf_fetch_select", "TUF metadata verification"),
                 "download": ("tuf_get", "TUF download"),
                 "extractor": ("tuf_extract", "TUF extraction")}
        changed = work / "changed-worker.wasm"
        for refused, (_, label) in roles.items():
            options = []
            for role, (name, _) in roles.items():
                original = tuf_workers / f"{name}.wasm"
                body = original.read_bytes()
                if role == refused:
                    changed.write_bytes(body + b"changed")
                options.extend((f"--{role}-module", changed if role == refused else original,
                                f"--{role}-pin", digest_of(body)))
            result = subprocess.run([*command, *options], env=fetch_env, capture_output=True, timeout=30)
            assert result.returncode != 0 and not output.exists(), result
            assert f"{label} module differs from its independent pin".encode() in result.stderr, result.stderr
        print("verified changed selector, downloader and extractor modules are refused", flush=True)
    grimoire("lock", "--toolchain")
    draft = grimoire("review", succeeds=False)
    assert "main.policy is missing" in draft.stdout
    grimoire("review", "--write-policy")
    reviewed = grimoire("review")
    assert "@lib" in reviewed.stdout and "@helper" in reviewed.stdout
    policy_file = project / "main.policy"
    policy = policy_file.read_bytes()
    policy_pin = digest_of(policy)
    inventory_file = work / "expected.json"
    toolchain_file = work / "audit-toolchain.json"
    toolchain_file.write_text(json.dumps(toolchain))
    audit = [python, Path(__file__).with_name("check_deployed_tuf_run.py"), inventory_file, project,
             "1", "--toolchain", toolchain_file, "--policy-pin", policy_pin]
    preflight = [*audit, "--preflight"]
    inventory_file.write_text(json.dumps(inventory))
    built_module = work / "installed-build.wasm"
    record_file = project / "run.json"
    output_file = project / "run.stdout"
    subprocess.run(preflight, env=env, check=True, capture_output=True, timeout=5)
    for path, wire, diagnostic in (
        (policy_file, policy, b"the local capability policy digest"),
        (project / "main.grants.toml", GRANT_DOCUMENT, b"the local grant document digest"),
    ):
        try:
            path.write_bytes(wire + b"\n")
            refused = subprocess.run(preflight, env=env, capture_output=True, timeout=5)
            assert refused.returncode == 1 and diagnostic in refused.stderr, refused
            assert all(not item.exists() for item in (built_module, record_file, output_file))
        finally:
            path.write_bytes(wire)
    print("deployment preflight refused changed policy and grants before any build or run", flush=True)
    grimoire("build", "-o", built_module)
    original_lock = (project / "witchy.lock").read_bytes()
    module_pin = digest_of(built_module.read_bytes())

    def check_run(version):
        inventory_file.write_text(json.dumps(inventory, indent=2, sort_keys=True) + "\n")
        result = grimoire("run", "--tuf-snapshot", str(version), snapshots[str(version)],
                          "--module-pin", module_pin, "--record", record_file,
                          "--launch-pins", inventory["manifest"], inventory["grants"],
                          state=f"clean-run-state-{version}")
        output_file.write_bytes(result.stdout.encode("utf-8"))
        check_deployed_run(inventory_file, project, version, toolchain, policy_pin, built_module=built_module)
        assert (project / "witchy.lock").read_bytes() == original_lock

    check_run(1)
    successful_record = record_file.read_bytes()
    for filename, before, after, diagnostic in (
        ("witchy.toml", b"output = true", b"output = false", "project manifest differs"),
        ("main.grants.toml", GRANT_DOCUMENT, GRANT_DOCUMENT + b"\n", "grant document differs"),
    ):
        path = project / filename
        approved = path.read_bytes()
        changed = approved.replace(before, after)
        assert changed != approved
        try:
            path.write_bytes(changed)
            refused = grimoire("run", "--launch-pins", inventory["manifest"], inventory["grants"],
                               "--module-pin", module_pin, "--record", record_file,
                               succeeds=False, state=f"refused-launch-pin-{filename}")
            assert diagnostic in refused.stderr and "--launch-pins" in refused.stderr, refused
            assert not refused.stdout and record_file.read_bytes() == successful_record
        finally:
            path.write_bytes(approved)
    print("Grimoire refused changed manifest and grant bytes before execution and preserved the successful record",
          flush=True)
    wrong_pin = digest_of(built_module.read_bytes() + b"changed")
    refused = grimoire("run", "--module-pin", wrong_pin, "--record", record_file,
                       succeeds=False, state="clean-refused-module-state")
    assert "rebuilt Wasm does not match --module-pin" in refused.stderr and not refused.stdout, refused
    assert record_file.read_bytes() == successful_record
    print("Grimoire refused a different module pin before running and preserved the successful record", flush=True)
    lock_file = project / "witchy.lock"
    try:
        lock_file.write_bytes(original_lock.replace(f'ca-pin = "{ca_pin}"\n'.encode(), b""))
        assert lock_file.read_bytes() != original_lock
        check_deployed_run(inventory_file, project, 1, toolchain, policy_pin)
    except ValueError as error:
        assert "the locked HTTPS CA bundle pin" in str(error), error
    else:
        raise AssertionError("deployment audit accepted a lock without the explicit CA pin")
    finally:
        lock_file.write_bytes(original_lock)
    for filename, diagnostic in (
        ("witchy.toml", "the local project manifest digest"),
        ("root.json", "the provisioned TUF root digest"),
        ("trust.json", "the provisioned release-trust digest"),
        ("ca.pem", "the provisioned HTTPS CA bundle digest"),
        ("main.wy", "the local project source digest of 'main.wy'"),
        ("local/value.wy", "the local project source digest of 'local/value.wy'"),
        ("main.grants.toml", "the local grant document digest"),
        ("main.policy", "the local capability policy digest"),
    ):
        path = project / filename
        original = path.read_bytes()
        changed = original[1:] + original[:1]
        assert changed != original
        path.write_bytes(changed)
        try:
            check_deployed_run(inventory_file, project, 1, toolchain, policy_pin)
        except ValueError as error:
            assert diagnostic in str(error), error
        else:
            raise AssertionError(f"deployment audit accepted a changed {filename}")
        finally:
            path.write_bytes(original)
    original_record = record_file.read_bytes()
    uncompleted = json.loads(original_record)
    del uncompleted["completed"]
    record_file.write_text(json.dumps(uncompleted))
    try:
        check_deployed_run(inventory_file, project, 1, toolchain, policy_pin)
    except ValueError as error:
        assert "the successful execution receipt" in str(error), error
    else:
        raise AssertionError("deployment audit accepted an unfinished run record")
    finally:
        record_file.write_bytes(original_record)
    unbound = json.loads(original_record)
    unbound["module"] = digest_of(b"different compiled module")
    record_file.write_text(json.dumps(unbound))
    try:
        check_deployed_run(inventory_file, project, 1, toolchain, policy_pin)
    except ValueError as error:
        assert "the module digest checked at host admission" in str(error), error
    else:
        raise AssertionError("deployment audit accepted a module digest different from the host pin")
    finally:
        record_file.write_bytes(original_record)
    changed_record = json.loads(original_record)
    changed_record["tuf"]["ca-pin"] = digest_of(b"another CA bundle")
    record_file.write_text(json.dumps(changed_record))
    try:
        check_deployed_run(inventory_file, project, 1, toolchain, policy_pin)
    except ValueError as error:
        assert "the recorded HTTPS CA bundle pin" in str(error), error
    else:
        raise AssertionError("deployment audit accepted a changed recorded CA pin")
    finally:
        record_file.write_bytes(original_record)
    manifest_file = project / "witchy.toml"
    original_manifest = manifest_file.read_bytes()
    try:
        default_ca_manifest = original_manifest.replace(b'ca-file = "ca.pem"\n', b"")
        assert default_ca_manifest != original_manifest
        manifest_file.write_bytes(default_ca_manifest)
        inventory["manifest"] = digest_of(default_ca_manifest)
        inventory_file.write_text(json.dumps(inventory))
        subprocess.run(preflight, env=env, check=True, capture_output=True, timeout=5)
        check_run(1)
        pinned_ca = inventory["registry"].pop("ca-pin")
        try:
            inventory_file.write_text(json.dumps(inventory))
            check_deployed_run(inventory_file, project, 1, toolchain, policy_pin)
        except ValueError as error:
            assert "the trusted registry identity fields and HTTPS CA pin" in str(error), error
        else:
            raise AssertionError("deployment audit accepted an inventory without the default CA pin")
        finally:
            inventory["registry"]["ca-pin"] = pinned_ca
        ca_file.write_bytes(fixture_roots + b"\n")
        inventory_file.write_text(json.dumps(inventory))
        refused = subprocess.run(preflight, env=env, capture_output=True, timeout=5)
        assert refused.returncode == 1 and b"the provisioned default HTTPS CA bundle digest" in refused.stderr
        refused = grimoire("build", succeeds=False, state="clean-changed-default-ca-state")
        assert "HTTPS CA bundle does not match its pin" in refused.stderr, refused.stderr
        rotated_pin = digest_of(ca_file.read_bytes())
        manifest_file.write_bytes(default_ca_manifest.replace(ca_pin.encode(), rotated_pin.encode()))
        refused = grimoire("build", succeeds=False, state="clean-different-locked-ca-state")
        assert "pins a different HTTPS CA bundle" in refused.stderr, refused.stderr
        assert lock_file.read_bytes() == original_lock
        grimoire("lock", state="clean-ca-rotation")
        rotated_lock = tomllib.loads(lock_file.read_text())
        expected_lock = tomllib.loads(original_lock.decode())
        expected_lock["tuf"]["ca-pin"] = rotated_pin
        assert rotated_lock == expected_lock
        grimoire("build", state="clean-new-ca-build")
        ca_file.write_bytes(fixture_roots)
        manifest_file.write_bytes(default_ca_manifest)
        grimoire("lock", state="clean-ca-rotation-restored")
        assert lock_file.read_bytes() == original_lock
        print("verified explicit CA rotation through lock and preserved the authenticated snapshot and package pins", flush=True)
    finally:
        ca_file.write_bytes(fixture_roots)
        manifest_file.write_bytes(original_manifest)
        inventory["manifest"] = manifest_pin
        inventory_file.write_text(json.dumps(inventory, indent=2, sort_keys=True) + "\n")
        record_file.write_bytes(original_record)

    audit = [*audit, "--built-module", built_module]
    original_module = built_module.read_bytes()
    try:
        built_module.write_bytes(original_module + b"changed")
        refused = subprocess.run(audit, capture_output=True, text=True, timeout=5)
        assert refused.returncode == 1 and "the retained build artifact digest" in refused.stderr, refused
        with built_module.open("wb") as stream:
            stream.truncate((16 << 20) + 1)
        refused = subprocess.run(audit, capture_output=True, text=True, timeout=5)
        assert refused.returncode == 1 and "the retained build artifact size" in refused.stderr, refused
    finally:
        built_module.write_bytes(original_module)
    accepted = subprocess.run(audit, check=True, capture_output=True, text=True, timeout=5)
    assert "verified retained build artifact" in accepted.stdout, accepted
    try:
        record_file.unlink()
        os.mkfifo(record_file)
        refused = subprocess.run(audit, capture_output=True, text=True, timeout=5)
        assert refused.returncode == 1 and "the local run record file type" in refused.stderr, refused
        record_file.unlink()
        with record_file.open("wb") as stream:
            stream.truncate((32 << 20) + 1)
        refused = subprocess.run(audit, capture_output=True, text=True, timeout=5)
        assert refused.returncode == 1 and "the local run record size" in refused.stderr, refused
    finally:
        record_file.unlink(missing_ok=True)
        record_file.write_bytes(original_record)
    print("deployment audit refused changed local inputs, trust anchors, a changed CA receipt and nonregular or oversized records",
          flush=True)
    tested = grimoire("test", "--projects", state="clean-project-test")
    assert "ok installed registry project" in tested.stdout
    publish(2)
    previous_record = record_file.read_bytes()
    for version, pin, diagnostic in (
        ("1", snapshots["1"], "differs from the expected digest"),
        ("1", snapshots["2"], "differs from the required TUF snapshot"),
    ):
        refused = grimoire("run", "--tuf-snapshot", version, pin, "--module-pin", module_pin,
                           "--record", record_file, succeeds=False, state=f"refused-snapshot-{pin[-12:]}")
        assert diagnostic in refused.stderr and not refused.stdout, refused
        assert record_file.read_bytes() == previous_record
    print("Grimoire refused a different signed snapshot and a mismatched version before running", flush=True)
    check_run(2)
    print("verified output 42, exact source and release digests, and both authenticated snapshots", flush=True)
    grimoire("lock")
    locked = (project / "witchy.lock").read_bytes()
    assert tomllib.loads(locked.decode())["tuf"]["snapshot-version"] == 2
    timestamp = mirror.mirror / "metadata/timestamp.json"
    timestamp.write_bytes(old_timestamp)
    result = grimoire("build", succeeds=False, state="clean-rollback-state")
    assert "rolls back" in result.stderr, result.stderr
    timestamp.write_bytes((mirror.metadata / "timestamp.json").read_bytes())
    snapshot_pin = tomllib.loads(locked.decode())["tuf"]["snapshot"]
    # Model an existing lock that recorded an inflated pre-recovery snapshot.
    poisoned_lock = locked.replace(b"snapshot-version = 2\n", b"snapshot-version = 40\n")
    poisoned_lock = poisoned_lock.replace(snapshot_pin.encode(), digest_of(b"pre-recovery snapshot").encode())
    assert poisoned_lock != locked
    lockfile = project / "witchy.lock"
    lockfile.write_bytes(poisoned_lock)
    result = grimoire("lock", succeeds=False, state="recovery-lock-state")
    assert "rolls back" in result.stderr and lockfile.read_bytes() == poisoned_lock
    result = grimoire("lock", "--recover-tuf-snapshot", digest_of(b"unavailable replacement"),
                      succeeds=False, state="recovery-lock-state")
    assert "differs from the expected digest" in result.stderr and lockfile.read_bytes() == poisoned_lock
    result = grimoire("lock", "--recover-tuf-snapshot", snapshot_pin, state="recovery-lock-state")
    assert f"recovered TUF snapshot {snapshot_pin} (version 2)" in result.stdout, result.stdout
    assert lockfile.read_bytes() == locked
    print("recovered the project lock through its independent snapshot pin and preserved every package pin", flush=True)
    target = next((mirror.mirror / "targets/lib").glob("*.coven"))
    original = target.read_bytes()
    target.write_bytes(original[:-1] + b" ")
    result = grimoire("build", succeeds=False, state="clean-tamper-state")
    assert "release evidence rejected" in result.stderr, result.stderr
    target.write_bytes(original)
    metadata = mirror.mirror / "metadata/2.snapshot.json"
    original = metadata.read_bytes()
    metadata.write_bytes(original[:-1] + b" ")
    result = grimoire("build", succeeds=False, state="clean-metadata-state")
    assert ("metadata verification refused" in result.stderr or "download refused" in result.stderr), result.stderr
    metadata.write_bytes(original)

    good_timestamp = timestamp.read_bytes()
    signed = Metadata.from_bytes(good_timestamp).signed
    mirror.issue("targets", signed, "unauthorized-timestamp.json")
    timestamp.write_bytes((mirror.metadata / "unauthorized-timestamp.json").read_bytes())
    result = grimoire("build", succeeds=False, state="clean-wrong-role-state")
    assert "metadata verification refused" in result.stderr, result.stderr
    timestamp.write_bytes(good_timestamp)

    root = project / "root.json"
    original = root.read_bytes()
    root.write_bytes(original + b" ")
    result = grimoire("build", succeeds=False, state="clean-root-pin-state")
    assert "initial TUF root does not match its pin" in result.stderr, result.stderr
    root.write_bytes(original)

    # The altered bundle still contains the root that validates the mirror.
    ca_file.write_bytes(fixture_roots[len(wrong_ca):] + wrong_ca)
    result = grimoire("build", succeeds=False, state="clean-wrong-ca-state")
    assert "HTTPS CA bundle does not match its pin" in result.stderr, result.stderr
    ca_file.write_bytes(fixture_roots)
    print("refused a different HTTPS CA bundle that still trusts the mirror", flush=True)

    untrusted = release.root / "untrusted-publisher"
    untrusted.mkdir()
    for filename in ("main.wy", "root.json", "ca.pem"):
        shutil.copyfile(project / filename, untrusted / filename)
    shutil.copytree(project / "local", untrusted / "local")
    restricted = json.loads(trust)
    del restricted["publishers"]["lib"]
    trust_wire = staging.canonical(restricted)
    (untrusted / "trust.json").write_bytes(trust_wire)
    manifest = (project / "witchy.toml").read_text().replace(mirror.args.trust_pin, digest_of(trust_wire))
    (untrusted / "witchy.toml").write_text(manifest)
    result = grimoire("lock", succeeds=False, state="clean-unauthorized-publisher", directory=untrusted)
    assert "release evidence rejected" in result.stderr, result.stderr
    assert not (untrusted / "witchy.lock").exists()

    assert (project / "witchy.lock").read_bytes() == locked
    print("refused rollback, changed bytes, wrong trust anchors and unauthorized metadata and publisher", flush=True)
    if web:
        deadline_context = fixture_tls_context(fullchain, tls_key) if tuf_workers is not None else None
        generation = verify_web(executable, mirror, project, port, env, grimoire, default_ca=True,
                                deadline_context=deadline_context)
        probe("catalog", expected=(generation / "index.html").stat().st_size)
        print("compiler-free mirror check verified the deployed catalog headers", flush=True)
        if chromium:
            spki = leaf_cert.public_key().public_bytes(serialization.Encoding.DER,
                                                      serialization.PublicFormat.SubjectPublicKeyInfo)
            pin = base64.b64encode(hashlib.sha256(spki).digest()).decode("ascii")
            subprocess.run(["node", Path(__file__).with_name("coven_browser_witness.mjs"),
                            chromium, f"https://localhost:{port}", pin], check=True, timeout=30)
        snapshot = publish(3)
        status, timestamp, _ = fetch_web(project, port, "/metadata/timestamp.json")
        assert status == 200 and timestamp == (mirror.mirror / "metadata/timestamp.json").read_bytes()
        target = next((mirror.mirror / "targets/lib").glob("*.coven"))
        status, body, _ = fetch_web(project, port, f"/targets/lib/{target.name}")
        assert status == 200 and body == target.read_bytes()
        verify_stale_catalog(project, port)
        probe("catalog", refusal=b"Witchy refused the mirror identity or route response")
        print("the mirror withheld stale web content and continued serving TUF metadata and packages", flush=True)
        generation = verify_web(executable, mirror, project, port, env, grimoire, snapshot_version=3)

        routes = mirror.mirror / "web/routes"
        previous_routes = routes.read_bytes()
        previous = json.loads((generation / "manifest.json").read_text())["catalog"]
        previous_timestamp_pin = digest_of((mirror.metadata / "timestamp.json").read_bytes())
        renewed_timestamp = mirror.issue("timestamp", Timestamp(version=7, expires=mirror.expiry, snapshot_meta=snapshot), "timestamp.json")
        renewed = mirror.publish()
        assert renewed["snapshot"] == 3 and renewed["timestamp"] == 7, renewed
        check_publication(snapshots["3"], previous_timestamp_pin,
                          refused="verified TUF timestamp differs from the expected digest")
        check_publication(snapshots["3"], "sha384:" + renewed_timestamp.hashes["sha384"])
        print("the deployment check distinguished a timestamp renewal at the same snapshot", flush=True)
        fresh_timestamp = (mirror.mirror / "metadata/timestamp.json").read_bytes()
        assert fresh_timestamp != timestamp
        assert fetch_web(project, port, "/metadata/timestamp.json", header="Cache-Control") == (200, fresh_timestamp, "no-store")
        assert fetch_web(project, port, f"/targets/lib/{target.name}")[:2] == (200, target.read_bytes())
        verify_stale_catalog(project, port)
        command = (executable, "web", "install", generation, "--mirror", mirror.mirror)
        refused = subprocess.run(command, env=env, capture_output=True, timeout=30)
        assert refused.returncode and b"differs from the current mirror publication" in refused.stderr, refused.stderr
        assert routes.read_bytes() == previous_routes

        grimoire("web", "build", state="clean-web-state", directory=generation.parent)
        current = json.loads((generation / "manifest.json").read_text())["catalog"]
        assert current == previous | {"timestamp": digest_of(fresh_timestamp)}, (previous, current)
        subprocess.run(command, env=env, check=True, capture_output=True, timeout=30)
        status, page, cache = fetch_web(project, port, "/", header="Cache-Control", content_type="text/html; charset=utf-8")
        assert (status, page, cache) == (200, (generation / "index.html").read_bytes(), "no-store")
        print("verified signed timestamp-only renewal withheld old routes and restored the same releases", flush=True)

    subprocess.run(verify, cwd=client_home, env=env, check=True, capture_output=True, timeout=30)
    print("the installed prefix still matches the exact pinned bundle after use", flush=True)


def fixture_chain(server):
    """Build a P-384 leaf, RSA intermediate, and a separate ECDSA trust anchor."""
    now = datetime.now(timezone.utc)

    def name(value):
        return x509.Name([x509.NameAttribute(x509.NameOID.COMMON_NAME, value)])

    def certificate(subject, issuer, key, signing_key, *, ca, path_length=None):
        return (x509.CertificateBuilder().subject_name(subject).issuer_name(issuer)
                .public_key(key.public_key()).serial_number(x509.random_serial_number())
                .not_valid_before(now - timedelta(minutes=5)).not_valid_after(now + timedelta(days=1))
                .add_extension(x509.BasicConstraints(ca=ca, path_length=path_length), True)
                .add_extension(x509.KeyUsage(digital_signature=not ca, content_commitment=False,
                                             key_encipherment=False, data_encipherment=False, key_agreement=False,
                                             key_cert_sign=ca, crl_sign=ca, encipher_only=None, decipher_only=None), True)
                .add_extension(x509.SubjectKeyIdentifier.from_public_key(key.public_key()), False)
                .add_extension(x509.AuthorityKeyIdentifier.from_issuer_public_key(signing_key.public_key()), False))

    root_key = ec.generate_private_key(ec.SECP384R1())
    root_name = name("Coven Test Root")
    root = certificate(root_name, root_name, root_key, root_key, ca=True, path_length=1).sign(root_key, hashes.SHA384())
    issuer_key = rsa.generate_private_key(public_exponent=65537, key_size=2048)
    issuer_name = name("Coven Test Intermediate")
    issuer = certificate(issuer_name, root_name, issuer_key, root_key, ca=True, path_length=0).sign(root_key, hashes.SHA384())
    leaf = (certificate(name("localhost"), issuer_name, server, issuer_key, ca=False)
            .add_extension(x509.ExtendedKeyUsage([x509.oid.ExtendedKeyUsageOID.SERVER_AUTH]), False)
            .add_extension(x509.SubjectAlternativeName([x509.DNSName("localhost"), x509.DNSName(LONG_PUBLIC_NAME)]), False)
            .sign(issuer_key, hashes.SHA256()))
    unrelated_key = ec.generate_private_key(ec.SECP384R1())
    unrelated_name = name("Coven Unrelated Test Root")
    unrelated = certificate(unrelated_name, unrelated_name, unrelated_key, unrelated_key, ca=True, path_length=0).sign(
        unrelated_key, hashes.SHA384())
    return leaf, issuer, root, unrelated


def export_fixture(destination, project, key, published, publish, inventory):
    """Keep the synthetic TLS key separate from the two public trees."""
    destination.mkdir(mode=0o700)
    try:
        shutil.copytree(project, destination / "project")
        shutil.copy2(key, destination / "mirror.der")
        for version, name in ((1, "published"), (2, "published-new")):
            if version == 2:
                publish(version)
            shutil.copytree(published, destination / name)
        (destination / "expected.json").write_text(json.dumps(inventory, indent=2, sort_keys=True) + "\n")
    except BaseException:
        shutil.rmtree(destination)
        raise
    print(f"exported the synthetic systemd fixture to {destination}", flush=True)


def verify_abandoned_download(build, work, project, fullchain, key):
    context = fixture_tls_context(fullchain, key)
    with socket.create_server(("127.0.0.1", 0)) as listener, ThreadPoolExecutor(max_workers=1) as pool:
        listener.settimeout(5)
        origin = urlsplit(f"https://localhost:{listener.getsockname()[1]}")

        def reply(extra):
            connection, _ = listener.accept()
            connection.settimeout(5)
            with context.wrap_socket(connection, server_side=True) as stream, stream.makefile("rb") as request:
                assert request.readline(1024) == b"GET /metadata/timestamp.json HTTP/1.1\r\n"
                assert request.readline(1024) == f"Host: {origin.netloc}\r\n".encode()
                assert request.readline(1024) == b"Connection: close\r\n"
                assert request.readline(1024) == b"Accept-Encoding: identity\r\n"
                assert request.readline(1024) == b"\r\n"
                stream.sendall(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\nx")
                if extra:
                    # Put the overrun in a separate TLS record.
                    stream.sendall(extra)
                # Closing without unwrap sends no TLS close_notify.

        destination = work / "staged-response"
        original = b"previous contents\n"
        destination.write_bytes(original)
        with downloader(build, HOST, origin, project / "ca.pem") as transport:
            for extra in (b"y", b""):
                peer = pool.submit(reply, extra)
                try:
                    transport.fetch("/metadata/timestamp.json", destination, max_length=1)
                except ValueError as error:
                    assert str(error) == "Witchy TUF download refused", error
                else:
                    raise AssertionError("invalid close-delimited download succeeded")
                finally:
                    peer.result(timeout=6)
                assert destination.read_bytes() == original
    print("late overflow and missing TLS close preserved the destination", flush=True)


def verify_ci_transport(build, work, project, fullchain, key):
    module = work / "ci-post.wasm"
    build("ci_post", module, TRANSPORT_BUILD)
    transport = (str(module), f"--module-pin={digest_of(module.read_bytes())}")
    context = fixture_tls_context(fullchain, key)
    expected = {"nonce": "a" * 43 + ".1." + "b" * 64}
    body = json.dumps(expected, separators=(",", ":")).encode()
    with socket.create_server(("127.0.0.1", 0)) as listener, ThreadPoolExecutor(max_workers=1) as pool:
        listener.settimeout(5)
        authority = f"localhost:{listener.getsockname()[1]}"

        def reply():
            connection, _ = listener.accept()
            connection.settimeout(5)
            with context.wrap_socket(connection, server_side=True) as stream:
                with stream.makefile("rb") as request:
                    assert request.readline(1024) == f"POST {CHALLENGE} HTTP/1.1\r\n".encode()
                    assert request.readline(1024) == f"Host: {authority}\r\n".encode()
                    assert request.readline(1024) == b"Connection: close\r\n"
                    assert request.readline(1024) == b"Content-Length: 0\r\n"
                    assert request.readline(1024) == b"\r\n"
                    stream.sendall(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n"
                                   + f"Content-Length: {len(body)}\r\n\r\n".encode() + body)
                    try:
                        stream.unwrap().close()
                    except ssl.SSLEOFError:
                        # The client consumed the authenticated close and closed its socket.
                        pass

        request = {"origin": f"https://{authority}", "route": CHALLENGE, "body_size": 0}
        ca_file = project / "ca.pem"
        ca_pin = digest_of(ca_file.read_bytes())
        changed = work / "changed-ci-ca.pem"
        changed.write_bytes(ca_file.read_bytes() + b"\n")
        try:
            post(HOST, transport, changed, authority, request, ca_pin=ca_pin)
        except ValueError as error:
            assert str(error) == "CI HTTPS CA bundle does not match its pin", error
        else:
            raise AssertionError("CI HTTPS accepted a changed staged CA bundle")
        listener.settimeout(0.15)
        try:
            connection, _ = listener.accept()
        except TimeoutError:
            pass
        else:
            connection.close()
            raise AssertionError("the CI worker connected before verifying its CA bundle")
        listener.settimeout(5)
        peer = pool.submit(reply)
        actual = post(HOST, transport, ca_file, authority, request, ca_pin=ca_pin)
        peer.result(timeout=6)
        assert actual == expected, actual
    print("CI HTTPS verified the CA pin before dialing and authenticated the challenge response", flush=True)


def verify_bodyless_routes(project, port):
    context = ssl.create_default_context(cafile=project / "ca.pem")
    with ExitStack() as stack:
        raw = stack.enter_context(socket.create_connection(("localhost", port), timeout=5))
        stream = stack.enter_context(context.wrap_socket(raw, server_hostname="localhost"))
        stream.sendall(f"GET /metadata/timestamp.json HTTP/1.1\r\nHost: localhost:{port}".encode())
        for byte in (b"\r", b"\n", b"\r", b"\n"):
            stream.sendall(byte)
        response = http.client.HTTPResponse(stream)
        stack.callback(response.close)
        response.begin()
        body = response.read()
        assert response.status == 200 and json.loads(body)["signed"]["_type"] == "timestamp"
        verify_tuf_headers(response, body, "application/json", "no-store")
    for fields in ({"Content-Length": "0"}, {"Expect": "100-continue"}):
        with ExitStack() as stack:
            conn = http.client.HTTPSConnection("localhost", port, context=context, timeout=5)
            stack.callback(conn.close)
            conn.request("GET", "/metadata/timestamp.json", headers=fields)
            response = conn.getresponse()
            assert response.status == 400 and response.read() == b"", fields

    start = f"GET /metadata/timestamp.json HTTP/1.1\r\nHost: localhost:{port}\r\n".encode()
    padded = start + b"X-Pad: "
    exact = padded + b"a" * (8192 - len(padded) - 4) + b"\r\n\r\n"
    invalid = (
        start.replace(b"HTTP/1.1", b"HTTP/1.0") + b"\r\n",
        start.replace(b"HTTP/1.1", b"HTTP/1.9") + b"\r\n",
        start.replace(b"GET /", b"GET https://localhost/") + b"\r\n",
    )
    cases = ((exact, 200), (exact[:-4] + b"a\r\n\r\n", None), (start + b"\r\nsuffix", 400))
    for request, expected in cases + tuple((wire, 400) for wire in invalid):
        with ExitStack() as stack:
            raw = stack.enter_context(socket.create_connection(("localhost", port), timeout=5))
            stream = stack.enter_context(context.wrap_socket(raw, server_hostname="localhost"))
            stream.sendall(request)
            if expected is None:
                assert stream.recv(1) == b"", "oversized mirror head received a response"
            else:
                response = http.client.HTTPResponse(stream)
                stack.callback(response.close)
                response.begin()
                assert response.status == expected, (len(request), response.status)
                response.read()


def verify_tuf_headers(response, body, content_type, cache):
    for name, value in (("Content-Length", str(len(body))), ("Content-Type", content_type), ("Connection", "close"),
                        ("Cache-Control", cache), ("X-Content-Type-Options", "nosniff"),
                        ("Content-Security-Policy", "default-src 'none'; frame-ancestors 'none'")):
        assert response.headers.get_all(name) == [value], name
    assert response.headers.get_all("Transfer-Encoding") is None


def verify_public_authority(project, port, published):
    authority = published / "tls/authority"
    previous = authority.read_bytes()
    context = ssl.create_default_context(cafile=project / "ca.pem")
    expected = (published / "metadata/timestamp.json").read_bytes()

    def request(host):
        with ExitStack() as stack:
            raw = stack.enter_context(socket.create_connection(("127.0.0.1", port), timeout=5))
            stream = stack.enter_context(context.wrap_socket(raw, server_hostname=LONG_PUBLIC_NAME))
            stream.sendall(f"GET /metadata/timestamp.json HTTP/1.1\r\nHost: {host}\r\n\r\n".encode("ascii"))
            response = http.client.HTTPResponse(stream)
            stack.callback(response.close)
            response.begin()
            return response.status, response.read()

    try:
        maximum = f"{LONG_PUBLIC_NAME}:65535\r\n".encode("ascii")
        assert len(maximum) == 261
        authority.write_bytes(maximum)
        assert request(f"{LONG_PUBLIC_NAME}:65535") == (200, expected)
        assert request(LONG_PUBLIC_NAME) == (421, b"")

        authority.write_bytes(f"{LONG_PUBLIC_NAME}:443\r\n".encode("ascii"))
        assert request(LONG_PUBLIC_NAME) == (200, expected)
        assert request(f"{LONG_PUBLIC_NAME}:443") == (200, expected)

        authority.write_bytes(f"{LONG_PUBLIC_NAME}:80\r\n".encode("ascii"))
        assert request(LONG_PUBLIC_NAME) == (421, b"")
        assert request(f"{LONG_PUBLIC_NAME}:80") == (200, expected)

        authority.write_bytes(maximum + b"\n")
        try:
            request(f"{LONG_PUBLIC_NAME}:65535")
        except (ssl.SSLEOFError, ConnectionResetError):
            pass
        else:
            raise AssertionError("mirror accepted an oversized authority file")
    finally:
        authority.write_bytes(previous)
    print("verified the maximum DNS name and port in the mirror authority file", flush=True)


def fetch_head(context, port, path, fields=()):
    # http.client suppresses a HEAD body when reading it. Check the TLS bytes directly.
    with socket.create_connection(("localhost", port), timeout=5) as raw:
        with context.wrap_socket(raw, server_hostname="localhost") as stream, stream.makefile("rb") as wire:
            request = (f"HEAD {path} HTTP/1.1\r\nHost: localhost:{port}\r\n"
                       + "".join(f"{key}: {value}\r\n" for key, value in fields) + "\r\n")
            stream.sendall(request.encode("ascii"))
            protocol, status, _ = wire.readline(160).split(b" ", 2)
            assert protocol == b"HTTP/1.1"
            headers = http.client.parse_headers(wire)
            assert wire.read(1) == b"", path
            return int(status), headers


def verify_tuf_responses(project, port, published):
    context = ssl.create_default_context(cafile=project / "ca.pem")
    target, = (published / "targets/helper").glob("*.coven")
    assert target.stat().st_size > 32768
    for path, content_type, cache in (
            (published / "metadata/timestamp.json", "application/json", "no-store"),
            (published / "metadata/1.snapshot.json", "application/json", "public, max-age=31536000, immutable"),
            (target, "application/octet-stream", "public, max-age=31536000, immutable")):
        with ExitStack() as stack:
            conn = http.client.HTTPSConnection("localhost", port, context=context, timeout=5)
            stack.callback(conn.close)
            route = "/" + path.relative_to(published).as_posix()
            conn.request("GET", route)
            response = conn.getresponse()
            body = response.read()
            assert response.status == 200 and body == path.read_bytes(), path
            verify_tuf_headers(response, body, content_type, cache)
        status, headers = fetch_head(context, port, route)
        assert status == 200 and list(headers.items()) == list(response.headers.items()), path
    for fields in ((("Content-Length", "1"),), (("Expect", "100-continue"),)):
        assert fetch_head(context, port, "/metadata/timestamp.json", fields)[0] == 400
    assert fetch_head(context, port, "/metadata/no-such-file.json")[0] == 404


def verify_oversized_objects(project, port, published):
    context = ssl.create_default_context(cafile=project / "ca.pem")
    for path, limit in ((published / "metadata/timestamp.json", 16384),
                        (published / "metadata/1.snapshot.json", 2097152),
                        (next((published / "targets/helper").glob("*.coven")), 2097152)):
        original = path.read_bytes()
        try:
            for size in (0, limit, limit + 1):
                body = b"x" * size
                path.write_bytes(body)
                with ExitStack() as stack:
                    connection = http.client.HTTPSConnection("localhost", port, context=context, timeout=5)
                    stack.callback(connection.close)
                    route = "/" + path.relative_to(published).as_posix()
                    connection.request("GET", route)
                    response = connection.getresponse()
                    if size <= limit:
                        assert response.status == 200 and response.read() == body, path
                    else:
                        assert response.status == 503, (path, response.status, response.getheader("Content-Length"))
                        assert response.headers.get_all("Cache-Control") == ["no-store"]
                        assert response.headers.get_all("Content-Length") == ["0"]
                        assert response.read() == b""
                status, headers = fetch_head(context, port, route)
                assert status == response.status and list(headers.items()) == list(response.headers.items()), path
        finally:
            path.write_bytes(original)
    print("verified empty TUF files, exact object limits and bodyless refusals for oversized files", flush=True)


def fetch_web(project, port, path, *, header="Content-Security-Policy", content_type=None):
    context = ssl.create_default_context(cafile=project / "ca.pem")
    with ExitStack() as stack:
        conn = http.client.HTTPSConnection("localhost", port, context=context, timeout=5)
        stack.callback(conn.close)
        conn.request("GET", path)
        response = conn.getresponse()
        body = response.read()
        if content_type is not None and response.status == 200:
            assert response.headers.get_all("Content-Length") == [str(len(body))], path
            assert response.headers.get_all("Content-Type") == [content_type], path
            assert response.headers.get_all("Connection") == ["close"], path
            assert response.headers.get_all("Transfer-Encoding") is None, path
            for name, expected in (("Cache-Control", "no-store"), ("X-Content-Type-Options", "nosniff"),
                                   ("Referrer-Policy", "no-referrer"), ("Cross-Origin-Resource-Policy", "same-origin")):
                assert response.headers.get_all(name) == [expected], (path, name)
            assert len(response.headers.get_all("Content-Security-Policy", ())) == 1, path
        return response.status, body, response.getheader(header)


def verify_stale_catalog(project, port):
    context = ssl.create_default_context(cafile=project / "ca.pem")
    for path in ("/", "/packages/glamour/0.3.0/", "/coven.css", "/glamour-browser.mjs",
                 "/glamour-worker.mjs", "/glamour-frame.html", "/filter.wasm"):
        assert fetch_web(project, port, path, header="Cache-Control") == (503, b"", "no-store"), path
        status, headers = fetch_head(context, port, path)
        assert status == 503 and headers.get_all("Cache-Control") == ["no-store"], path


def verify_web(coven, mirror, project, port, env, grimoire, *, snapshot_version=2, default_ca=False, deadline_context=None):
    site = project / f"site-{snapshot_version}"
    def command(*args):
        return subprocess.run((coven, *args), env=env, timeout=60, check=True, capture_output=True)
    command("web", "init-tuf", site, "--registry", f"https://localhost:{port}", "--id", "test",
            "--root", project / "root.json", "--root-pin", mirror.args.root_pin,
            "--trust", project / "trust.json", "--trust-pin", mirror.args.trust_pin,
            "--ca-file", project / "ca.pem", "--ca-pin", digest_of((project / "ca.pem").read_bytes()))
    for folder in (site, site / "browser"):
        manifest = folder / "witchy.toml"
        registry = tomllib.loads(manifest.read_text())["registry"]
        assert registry["ca-pin"] == digest_of((folder / registry["ca-file"]).read_bytes())
        if default_ca:
            manifest.write_text("".join(line for line in manifest.read_text().splitlines(keepends=True)
                                        if not line.startswith(("ca-file =", "ca-pin ="))))
    for directory in (site, site / "browser"):
        grimoire("lock", "--toolchain", directory=directory)
        grimoire("review", "--write-policy", directory=directory)
    if deadline_context is not None:
        verify_interrupted_tuf(coven.with_name("grimoire"), env, project, site, deadline_context,
                               deadline_ms=5000, web=True)
        verify_web_input_changes(coven, site, env)
    if default_ca:
        manifest = site / "browser/witchy.toml"
        original = manifest.read_text()
        (manifest.parent / "alternate.pem").write_bytes((project / "ca.pem").read_bytes() + b"\n")
        manifest.write_text(original.replace('[registry]\n', '[registry]\nca-file = "alternate.pem"\n', 1))
        refused = grimoire("web", "build", succeeds=False, state="alternate-web-ca", directory=site)
        assert "HTTPS CA bundle does not match its pin" in refused.stderr, refused.stderr
        assert not os.path.lexists(site / "dist")
        manifest.write_text(original)
        print("Glamour refused a browser build using different PEM bytes from the catalog", flush=True)
    grimoire("web", "build", state="clean-web-state", directory=site)
    generation = site / "dist"
    command("web", "check", generation)
    command("web", "install", generation, "--mirror", mirror.mirror)

    context = ssl.create_default_context(cafile=project / "ca.pem")

    def get(path, content_type="text/html; charset=utf-8"):
        status, body, policy = fetch_web(project, port, path, content_type=content_type)
        assert status == 200, (path, status)
        status, headers = fetch_head(context, port, path)
        assert status == 200, (path, status)
        assert headers.get_all("Content-Length") == [str(len(body))], path
        assert headers.get_all("Content-Type") == [content_type], path
        assert headers.get_all("Cache-Control") == ["no-store"], path
        assert headers.get_all("Content-Security-Policy") == [policy], path
        return body, policy

    receipt = json.loads((generation / "manifest.json").read_text())
    assert receipt["format"] == "coven-web-7"
    assert receipt["catalog"]["snapshot_version"] == snapshot_version
    page, policy = get("/")
    assert b"Package registry" in page and b'/packages/glamour/0.3.0/' in page
    assert "require-trusted-types-for 'script'" in policy and "frame-ancestors 'none'" in policy
    assert "script-src 'sha384-" in policy and "trusted-types 'none'" in policy
    assert "frame-src 'self'" in policy and "worker-src 'none'" in policy
    frame, policy = get("/glamour-frame.html")
    assert digest_of(frame) == receipt["assets"]["glamour-frame.html"]
    script = frame.split(b"<script>", 1)[1].split(b"</script>", 1)[0]
    pin = base64.b64encode(hashlib.sha384(script).digest()).decode("ascii")
    assert f"script-src 'sha384-{pin}' 'wasm-unsafe-eval'" in policy
    assert "sandbox allow-scripts" in policy and "worker-src blob:" in policy
    assert "default-src 'none'" in policy and "frame-ancestors 'self'" in policy
    assert "allow-same-origin" not in policy
    detail, policy = get("/packages/glamour/0.3.0/")
    assert b"Sealed target digest" in detail and b"<script" not in detail and "trusted-types 'none'" in policy
    worker, policy = get("/glamour-worker.mjs", "text/javascript; charset=utf-8")
    assert digest_of(worker) == receipt["assets"]["glamour-worker.mjs"]
    assert "'wasm-unsafe-eval'" in policy and "trusted-types 'none'" in policy
    for filename, media_type in (("coven.css", "text/css; charset=utf-8"),
                                 ("glamour-browser.mjs", "text/javascript; charset=utf-8"),
                                 ("filter.wasm", "application/wasm")):
        body, _ = get(f"/{filename}", media_type)
        assert body == (generation / filename).read_bytes(), filename
    verify_web_asset_pins(coven, generation, mirror.mirror, project, port, env)
    verify_web_route_table(mirror.mirror, project, port)
    print(f"installed and served the checked Glamour catalog for snapshot {snapshot_version}", flush=True)
    return generation


def verify_web_input_changes(coven, site, env):
    changed = site.with_name("changed-web-inputs")
    shutil.copytree(site, changed)
    wrapper = changed / "edit-after-catalog"
    marker = changed / "catalog-returned"
    files = [changed / name for name in ("witchy.toml", "render.grants.toml", "witchy.lock",
                                        "browser/witchy.toml", "browser/filter.grants.toml", "browser/witchy.lock")]
    wrapper.write_text(f"""#!/usr/bin/env python3
import os
from pathlib import Path
import subprocess
import sys

fd = os.environ.get("WITCHY_COVEN_LIFELINE_FD")
result = subprocess.run([{str(coven)!r}, *sys.argv[1:]], pass_fds=(int(fd),) if fd is not None else ())
marker = Path({str(marker)!r})
if sys.argv[1:3] == ["tuf", "catalog"] and result.returncode == 0 and not marker.exists():
    for name in {list(map(str, files))!r}:
        Path(name).write_bytes(b"[invalid")
    marker.touch()
raise SystemExit(result.returncode)
""")
    wrapper.chmod(0o700)
    local_env = env | {"COVEN": str(wrapper), "XDG_STATE_HOME": str(changed / "state")}
    result = subprocess.run([coven.with_name("grimoire"), "web", "build", changed],
                            env=local_env, capture_output=True, text=True, timeout=90)
    assert marker.exists(), result.stderr
    assert result.returncode == 1 and "web build input changed" in result.stderr, result.stderr
    assert not os.path.lexists(changed / "dist") and not list(changed.glob(".coven-web-*"))
    print("web compilation used the checked project inputs and refused publication after their replacement", flush=True)


def catalog_route_table(profile, timestamp, rows):
    body = b"".join(rows)
    if profile == b"coven-web-https-4\n":
        return profile + timestamp + hashlib.sha384(body).hexdigest().encode() + b"\n" + body
    return profile + timestamp + body


def verify_web_route_table(published, project, port):
    routes = published / "web/routes"
    table = routes.read_bytes()
    profile, timestamp, checksum, *rows = table.splitlines(keepends=True)
    assert profile == b"coven-web-https-4\n" and table == catalog_route_table(profile, timestamp, rows)
    frame = next(row for row in rows if row.startswith(b"/glamour-frame.html "))
    package = next(row for row in rows if row.startswith(b"/packages/"))
    digest = package.split()[1]
    legacy = b"coven-web-https-2\n"
    variants = {
        "missing fixed route": [row for row in rows if row != frame],
        "duplicate package route": sorted(rows + [package]),
        "out-of-order routes": list(reversed(rows)),
        "unknown route": sorted(rows + [b"/unknown " + digest + b"\n"]),
        "oversized package inventory": sorted([row for row in rows if not row.startswith(b"/packages/")]
                                               + [f"/packages/lib/{i}.0.0/ ".encode() + digest + b"\n"
                                                  for i in range(65)]),
    }
    try:
        routes.write_bytes(profile + timestamp + checksum + b"".join(row for row in rows if row != package))
        assert fetch_web(project, port, "/", header="Cache-Control") == (503, b"", "no-store")
        for label, changed in variants.items():
            routes.write_bytes(catalog_route_table(profile, timestamp, changed))
            for route in ("/", "/filter.wasm"):
                assert fetch_web(project, port, route, header="Cache-Control") == (503, b"", "no-store"), label
            status, body, _ = fetch_web(project, port, "/metadata/timestamp.json", header="Cache-Control")
            assert status == 200 and body == (published / "metadata/timestamp.json").read_bytes(), label
        routes.write_bytes(catalog_route_table(legacy, timestamp, rows))
        assert fetch_web(project, port, "/", header="Cache-Control") == (503, b"", "no-store")
        routes.write_bytes(catalog_route_table(legacy, timestamp, [row for row in rows if row != frame]))
        status, _, csp = fetch_web(project, port, "/")
        assert status == 200 and "worker-src 'self'" in csp
        assert fetch_web(project, port, "/glamour-frame.html", header="Cache-Control") == (404, b"", "no-store")
        routes.write_bytes(catalog_route_table(b"coven-web-https-3\n", timestamp, rows))
        status, _, csp = fetch_web(project, port, "/")
        assert status == 200 and "worker-src 'none'" in csp
    finally:
        routes.write_bytes(table)
    status, _, csp = fetch_web(project, port, "/")
    assert status == 200 and "worker-src 'none'" in csp
    print("mirror refused invalid catalog tables, preserved TUF routes and admitted complete current and legacy profiles", flush=True)


def verify_web_asset_pins(coven, generation, published, project, port, env):
    manifest = generation / "manifest.json"
    receipt_wire = manifest.read_bytes()
    routes = published / "web/routes"
    table = routes.read_bytes()
    for name in ("glamour-browser.mjs", "glamour-worker.mjs", "glamour-frame.html"):
        path, route = generation / name, f"/{name}"
        original = path.read_bytes()
        changed = original + b"\n"
        digest = digest_of(changed)
        receipt = json.loads(receipt_wire)
        old_digest = receipt["assets"][name]
        receipt["assets"][name] = digest
        try:
            path.write_bytes(changed)
            manifest.write_text(json.dumps(receipt))
            refused = subprocess.run((coven, "web", "install", generation, "--mirror", published),
                                     env=env, timeout=30, capture_output=True)
            assert refused.returncode and f"{name} failed digest verification".encode() in refused.stderr
            assert routes.read_bytes() == table
        finally:
            path.write_bytes(original)
            manifest.write_bytes(receipt_wire)

        substitute = published / "web/assets" / digest.removeprefix("sha384:")
        previous = f"{route} {old_digest.removeprefix('sha384:')}\n".encode()
        replacement = f"{route} {digest.removeprefix('sha384:')}\n".encode()
        assert previous in table
        with substitute.open("xb") as output:
            output.write(changed)
        try:
            profile, timestamp, _, *rows = table.splitlines(keepends=True)
            routes.write_bytes(catalog_route_table(profile, timestamp, [replacement if row == previous else row for row in rows]))
            checked = (route, "/") if name == "glamour-browser.mjs" else (route,)
            for target in checked:
                assert fetch_web(project, port, target, header="Cache-Control") == (503, b"", "no-store")
        finally:
            routes.write_bytes(table)
            substitute.unlink()


if __name__ == "__main__":
    cli = argparse.ArgumentParser(description=__doc__)
    cli.add_argument("--web", action="store_true", help="also build, install, and serve the Glamour catalog")
    cli.add_argument("--chromium", type=Path, help="also verify the served catalog in this Chromium executable")
    cli.add_argument("--client-lean-root", type=Path, help="use a separate full Lean SDK for the installed client")
    cli.add_argument("--client-python", type=Path, help="directory containing the detached client's python3")
    cli.add_argument("--tuf-workers", type=Path, help="include the checked TUF workers in the installed client bundle")
    cli.add_argument("--manifest-worker", type=Path, help="include the checked manifest worker in the installed client bundle")
    cli.add_argument("--platform-ca-file", type=Path, help="append a platform PEM bundle to the synthetic fixture roots")
    cli.add_argument("--export-systemd-fixture", type=Path,
                     help="write two synthetic publications and a localhost consumer project to a new directory")
    cli.add_argument("--fixture-port", type=int,
                     help="loopback HTTPS port in the exported project (default 18443)")
    cli.add_argument("--fixture-default-ca", action="store_true",
                     help="pin the exported CA bundle without placing its path in the manifest")
    options = cli.parse_args()
    if options.fixture_default_ca and options.export_systemd_fixture is None:
        cli.error("--fixture-default-ca requires fixture export")
    if options.export_systemd_fixture is not None:
        if any((options.chromium, options.client_lean_root, options.client_python, options.tuf_workers, options.manifest_worker)):
            cli.error("fixture export does not install or run a client")
        if os.path.lexists(options.export_systemd_fixture):
            cli.error("--export-systemd-fixture requires a new directory")
    if options.fixture_port is not None:
        if options.export_systemd_fixture is None or not 1 <= options.fixture_port <= 65535:
            cli.error("--fixture-port requires fixture export and a port from 1 to 65535")
    if options.client_python is not None:
        options.client_python = options.client_python.resolve()
        python = options.client_python / "python3"
        if not python.is_file() or not os.access(python, os.X_OK):
            cli.error("--client-python must contain an executable python3")
    with ExitStack() as resources:
        witness(resources, web=options.web or options.chromium is not None, chromium=options.chromium,
                client_lean_root=options.client_lean_root, client_python=options.client_python,
                tuf_workers=options.tuf_workers, manifest_worker=options.manifest_worker,
                platform_ca_file=options.platform_ca_file,
                export_directory=options.export_systemd_fixture,
                fixture_port=options.fixture_port or 18443, fixture_default_ca=options.fixture_default_ca)
