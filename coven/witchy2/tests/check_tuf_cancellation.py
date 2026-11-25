"""Check installed Grimoire cancellation using a disposable Coven TLS fixture."""
import argparse
import base64
import os
from pathlib import Path
import select
import shutil
import signal
import socket
import ssl
import subprocess
import sys
import tempfile
import time
import tomllib


def require(condition, message):
    if not condition:
        raise ValueError(message)


def fixture_tls_context(fullchain, key_der):
    with tempfile.TemporaryDirectory(prefix="witchy-fixture-tls-key-") as directory:
        work = Path(directory)
        key = work / "key.pem"
        with os.fdopen(os.open(key, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600), "wb") as stream:
            stream.write(b"-----BEGIN PRIVATE KEY-----\n")
            stream.write(base64.encodebytes(key_der.read_bytes()))
            stream.write(b"-----END PRIVATE KEY-----\n")
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        context.minimum_version = ssl.TLSVersion.TLSv1_3
        context.set_ecdh_curve("secp384r1")
        context.set_alpn_protocols(["http/1.1"])
        context.load_cert_chain(fullchain, key)

    return context


def verify_interrupted_tuf(grimoire, env, work, project, context, *, deadline_ms=None, web=False):
    staging = work / ("expired-catalog" if web else "cancelled-fetch" if deadline_ms is None else "expired-fetch")
    staging.mkdir(mode=0o700)

    # Use fresh state so the first download is a request for the second root.
    env = env | {"TMPDIR": str(staging), "XDG_STATE_HOME": str(staging / "state"),
                 "XDG_CACHE_HOME": str(staging / "cache")}
    with socket.create_server(("127.0.0.1", 0)) as listener:
        listener.settimeout(6)
        port = listener.getsockname()[1]
        isolated = staging / "project"
        shutil.copytree(project, isolated)
        projects = (isolated, isolated / "browser") if web else (isolated,)
        for directory in projects:
            manifest = directory / "witchy.toml"
            config = manifest.read_text()
            origin = tomllib.loads(config)["registry"]["url"]
            before, after = f'url = "{origin}"', f'url = "https://localhost:{port}"'
            config = config.replace(before, after)
            if deadline_ms is not None and directory == isolated:
                config = config.replace("[registry]\n", f"[registry]\nresolution-deadline-ms = {deadline_ms}\n")
            manifest.write_text(config)
            if web:
                lock = directory / "witchy.lock"
                lock.write_text(lock.read_text().replace(before, after))
        action = ("web", "build") if web else ("lock", "--toolchain")
        with subprocess.Popen([grimoire, *action, isolated], env=env, start_new_session=True, stderr=subprocess.PIPE) as launcher:
            try:
                try:
                    connection, _ = listener.accept()
                except TimeoutError as error:
                    if launcher.poll() is not None:
                        _, diagnostic = launcher.communicate()
                        raise ValueError(f"Grimoire exited before opening HTTPS: {diagnostic!r}") from error
                    raise
                connection.settimeout(2)
                with context.wrap_socket(connection, server_side=True) as stream, stream.makefile("rb") as reader:
                    expected = (b"GET /metadata/2.root.json HTTP/1.1\r\n", f"Host: localhost:{port}\r\n".encode(),
                                b"Connection: close\r\n", b"Accept-Encoding: identity\r\n", b"\r\n")
                    for line in expected:
                        actual = reader.readline(1024)
                        require(actual == line, f"unexpected HTTPS request line {actual!r}, expected {line!r}")
                    stream.sendall(b"HTTP/1.1 200 OK\r\nContent-Length: 64\r\n\r\nx")
                    if deadline_ms is None:
                        launcher.kill()
                        require(launcher.wait(timeout=2) == -signal.SIGKILL, "Grimoire did not receive SIGKILL")
                    else:
                        # Keep sending a partial body so the socket's two-second idle timeout
                        # cannot mask the resolution deadline.
                        until = time.monotonic() + deadline_ms / 1000 + 1
                        while time.monotonic() < until:
                            if select.select([stream], [], [], 0.25)[0]:
                                break
                            stream.sendall(b"x")
                        _, diagnostic = launcher.communicate(timeout=1)
                        expected = (b"verified catalog exceeded the build deadline" if web else
                                    b"TUF dependency resolution exceeded [registry] resolution-deadline-ms")
                        require(launcher.returncode == 1 and expected in diagnostic,
                                f"missing deadline refusal: {diagnostic!r}")
                    stream.settimeout(1)
                    require(reader.read(1) == b"", "HTTPS socket outlived Grimoire")
                if web:
                    require(not os.path.lexists(isolated / "dist"), "expired catalog published a generation")
                else:
                    require(not (isolated / "witchy.lock").exists(), "cancelled lock published a lockfile")
                require(not any(staging.rglob("release")), "cancelled lock published a release")
                require(not any(staging.rglob("2.root.json")), "cancelled lock published a root revision")
            except BaseException:
                try:
                    os.killpg(launcher.pid, signal.SIGKILL)
                except (ProcessLookupError, PermissionError):
                    pass
                raise
    if web:
        print("installed Glamour catalog deadline closed HTTPS without publishing a generation", flush=True)
    else:
        reason = "cancellation" if deadline_ms is None else "resolution deadline"
        print(f"installed Grimoire {reason} closed HTTPS without publishing a lock, metadata or a release", flush=True)


def check(fixture, grimoire):
    with tempfile.TemporaryDirectory(prefix="witchy-tuf-cancellation-") as temporary:
        work = Path(temporary).resolve()
        chain = (fixture / "published/tls/chain").read_bytes()
        certificates = []
        while chain:
            size = int.from_bytes(chain[:3], "big")
            require(0 < size <= len(chain) - 3, "invalid fixture certificate length")
            certificates.append(ssl.DER_cert_to_PEM_cert(chain[3:3 + size]))
            chain = chain[3 + size:]
        require(bool(certificates), "missing fixture certificate chain")
        fullchain = work / "fullchain.pem"
        fullchain.write_text("".join(certificates), encoding="ascii")
        context = fixture_tls_context(fullchain, fixture / "mirror.der")

        home = work / "home"
        home.mkdir(mode=0o700)
        path = os.pathsep.join((str(grimoire.parent), str(Path(sys.executable).absolute().parent), os.defpath))
        env = {"HOME": str(home), "PATH": path}
        verify_interrupted_tuf(grimoire, env, work, fixture / "project", context)
        verify_interrupted_tuf(grimoire, env, work, fixture / "project", context, deadline_ms=5000)


if __name__ == "__main__":
    cli = argparse.ArgumentParser(description=__doc__)
    cli.add_argument("fixture", type=Path, help="directory from --export-systemd-fixture")
    cli.add_argument("grimoire", type=Path, help="path to the installed Grimoire executable")
    args = cli.parse_args()
    check(args.fixture.resolve(strict=True), args.grimoire.absolute())
