"""Give a pinned Witchy listener one private action channel and a TLS signer."""
from contextlib import contextmanager
import os
import re
import selectors
import socket
import stat
import subprocess
import time

from coven_permissions import extended_acl


LISTENER = re.compile(r"(?:127\.0\.0\.1|\[::1\]):([1-9][0-9]{0,4})")


def require_loopback(address):
    listener = LISTENER.fullmatch(address)
    if listener is None or int(listener[1]) > 65535:
        raise ValueError("listener requires 127.0.0.1:PORT or [::1]:PORT")


def readiness_destination():
    destination = os.environ.get("NOTIFY_SOCKET")
    if not destination:
        return None
    if destination.startswith("@") and len(destination) > 1:
        return "\0" + destination[1:]
    if not destination.startswith("/"):
        raise ValueError("invalid service readiness socket")
    return destination


def notify_ready(process, destination):
    if destination is None:
        return
    marker = b"READY\n"
    deadline = time.monotonic() + 10
    received = b""
    with process.stdout as source, selectors.DefaultSelector() as readable:
        readable.register(source, selectors.EVENT_READ)
        while len(received) < len(marker):
            remaining = deadline - time.monotonic()
            if remaining <= 0 or not readable.select(remaining):
                break
            chunk = os.read(source.fileno(), len(marker) + 1 - len(received))
            if not chunk:
                break
            received += chunk
            if not marker.startswith(received):
                break
        if received != marker or process.poll() is not None:
            raise ValueError("Witchy listener did not become ready")
    with socket.socket(socket.AF_UNIX, socket.SOCK_DGRAM) as notifier:
        notifier.settimeout(1)
        notifier.sendto(b"READY=1", destination)


@contextmanager
def private_tls_key(path):
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC)
    try:
        status = os.fstat(fd)
        if (not stat.S_ISREG(status.st_mode) or status.st_uid != os.getuid() or status.st_nlink != 1
                or status.st_mode & 0o077 or not 1 <= status.st_size <= 16384 or extended_acl(fd)):
            raise ValueError("TLS key must be a private PKCS#8 file owned by the service")
        yield fd
    finally:
        os.close(fd)


def relay(channel, dispatch, request_limit, reply_limit):
    with channel.makefile("rb") as source:
        while header := source.read(4):
            size = int.from_bytes(header, "big")
            if len(header) != 4 or not 1 <= size <= request_limit:
                raise ValueError("invalid request length on the private HTTPS descriptor")
            data = source.read(size)
            if len(data) != size:
                raise ValueError("incomplete request on the private HTTPS descriptor")
            reply = dispatch(data)
            if not 1 <= len(reply) <= reply_limit:
                reply = b"\2"
            channel.sendall(len(reply).to_bytes(4, "big") + reply)


def run_listener(host, wasm, listen, tls_key, public, dispatch, request_limit, reply_limit, flags, *, module_pin):
    ready = readiness_destination()
    client, service = socket.socketpair()
    alive, lifetime = os.pipe()
    try:
        with client, service, private_tls_key(tls_key) as issuer:
            fd = client.fileno()
            command = (host, os.path.join(os.getcwd(), wasm), f"--module-pin={module_pin}", "--allocator", "--rand", "--no-links", f"--listen={listen}",
                       f"--dir={public}", "--instance-per-connection", f"--local-service-fd={fd}",
                       f"--secret-slot=issuer:sign/tls13/server-p384:fd:{issuer}", f"--lifeline=fd:{alive}",
                       "--deadline-ms=none", *flags)
            if ready is not None:
                command += ("--listener-ready-stdout",)
            process = subprocess.Popen(command, stdin=subprocess.DEVNULL,
                                       stdout=subprocess.PIPE if ready is not None else None,
                                       pass_fds=(fd, issuer, alive), env={})
            client.close()
            try:
                notify_ready(process, ready)
                relay(service, dispatch, request_limit, reply_limit)
                raise ValueError("Witchy HTTPS action channel closed")
            finally:
                if process.poll() is None:
                    process.terminate()
                try:
                    process.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
    finally:
        os.close(lifetime)
        os.close(alive)
