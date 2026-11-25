"""Deliver the transport cache to a read-only Witchy target selector."""
from contextlib import contextmanager
import os
import re
import secrets
import subprocess

from coven_staging import canonical, read_file
from coven_worker import check_worker_pin, run_with_lifeline, worker_arguments
from witchy_release import DIGEST, NAME, VERSION, digest_of

ROOT_LINK = re.compile(r"root_history/([1-9][0-9]{0,15})\.root\.json")
PIN_LINE = rb"(sha384:[0-9a-f]{96})\n"
TARGET_ROUTE = rb"(/targets/[a-z][a-z0-9-]{0,63}/[0-9a-f]{96}\.[0-9.]{5,29}\.coven)\n"
SNAPSHOT_VERSION = rb"([1-9][0-9]{0,15})"
VERDICT = re.compile(rb"verified\n" + PIN_LINE * 2 + rb"([1-9][0-9]{0,6})\n" + TARGET_ROUTE
                     + SNAPSHOT_VERSION + rb"\n" + PIN_LINE * 2)
ABSENT = re.compile(rb"absent\n" + PIN_LINE * 3)
HISTORY = "coven-fetch-history"
TIMESTAMP_FLOOR = "coven-fetch-timestamp"
TIMESTAMP_LIMIT = 16384
TIMESTAMP_FLOOR_BYTES = TIMESTAMP_LIMIT + 17
HISTORY_BYTES = (2 << 20) + TIMESTAMP_LIMIT + 192
CATALOG_MAX_ROLES = 64
CATALOG_MAX_RELEASES = 4096
CATALOG_METADATA_BYTES = 32 << 20
CATALOG_ROW = re.compile(rb"([a-z][a-z0-9-]{0,63})/([0-9.]{5,29})\.coven (sha384:[0-9a-f]{96}) ([1-9][0-9]{0,6})")
MODULE_BYTES = 262144
BUILD = ("--heap-pages=448", "--slots=65536", "--stack-bytes=65536")


def latest_root(cache):
    try:
        link = os.readlink("root.json", dir_fd=cache)
    except FileNotFoundError:
        return None
    latest = ROOT_LINK.fullmatch(link)
    if latest is None or int(latest[1]) >= 2**53:
        raise ValueError("invalid cached TUF root history")
    return int(latest[1])


@contextmanager
def selector_worker(build, host, root, trust, request, *, source=None, pin=None):
    catalog = request["coordinate"] is None
    with worker_arguments(build, "tuf_fetch_select", BUILD, MODULE_BYTES, "TUF metadata verification",
                          source=source, pin=pin) as module_args:
        def run(cache, version, mode):
            header = canonical(request | {"root_version": version, "root_size": len(root),
                                          "trust_size": len(trust), "mode": mode}) + b"\n"
            metadata_bytes = 5 * (2 << 20) + (CATALOG_METADATA_BYTES if catalog else 0)
            size = min((version - 1) * 512000 + metadata_bytes + 32768 + HISTORY_BYTES + TIMESTAMP_FLOOR_BYTES, 1 << 40)
            output = CATALOG_MAX_RELEASES * 216 + 1024 if catalog else 768
            command = (host, *module_args, f"--dir={cache.resolve(strict=True)}", "--no-links", "--allocator", "--clock",
                       "--input-stdin", f"--input-bytes={512000 + 262144 + 1024}", "--output", f"--output-bytes={output}",
                       f"--fs-bytes={size}", f"--module-bytes={MODULE_BYTES}", "--memory-pages=768",
                       "--fuel=16000000000", "--deadline-ms=20000")
            try:
                result = run_with_lifeline(subprocess.run, command, input=header + root + trust,
                                           capture_output=True, timeout=22)
            except (OSError, subprocess.TimeoutExpired):
                raise ValueError("Witchy TUF metadata verification unavailable") from None
            check_worker_pin(result, "TUF metadata verification")
            if result.returncode or result.stdout == b"refused\n":
                raise ValueError("Witchy TUF metadata verification refused the target")
            if result.stdout == b"refused rollback\n":
                raise ValueError("Witchy TUF metadata rollback refused")
            return result.stdout

        yield run


def select_target(build, host, cache, root, trust, request):
    fd = os.open(cache, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        version = latest_root(fd) or 1
        with selector_worker(build, host, root, trust, request) as run:
            return finish_selection(run(cache, version, "select"), cache, fd, version, root)
    finally:
        os.close(fd)


def finish_selection(verdict, source, cache, version, root):
    absent = ABSENT.fullmatch(verdict)
    if absent is not None:
        snapshot, timestamp, latest_pin = (field.decode("ascii") for field in absent.groups())
        persist_history(source, cache, version, root, latest_pin, timestamp, snapshot)
        return None
    matched = VERDICT.fullmatch(verdict)
    if matched is None:
        raise ValueError("Witchy TUF metadata verification refused the target")
    snapshot, target, length, route, number, timestamp, latest_pin = (field.decode("ascii") for field in matched.groups())
    snapshot_version = int(number)
    if snapshot_version >= 2**53:
        raise ValueError("invalid verified TUF snapshot version")
    persist_history(source, cache, version, root, latest_pin, timestamp, snapshot)
    return snapshot, target, int(length), route, snapshot_version


def finish_catalog(verdict, source, cache, version, root, registry):
    lines = verdict.split(b"\n")
    if not 6 <= len(lines) <= CATALOG_MAX_RELEASES + 6 or lines[0] != b"catalog" or lines[-1]:
        raise ValueError("invalid Witchy TUF catalog result")
    if not re.fullmatch(SNAPSHOT_VERSION, lines[1]) or (snapshot_version := int(lines[1])) >= 2**53:
        raise ValueError("invalid Witchy TUF catalog snapshot version")
    snapshot, timestamp, latest_pin = (field.decode("ascii") for field in (lines[2], lines[-3], lines[-2]))
    if not all(DIGEST.fullmatch(pin) for pin in (snapshot, timestamp, latest_pin)):
        raise ValueError("invalid Witchy TUF catalog metadata pins")
    entries = {}
    for line in lines[3:-3]:
        matched = CATALOG_ROW.fullmatch(line)
        if matched is None:
            raise ValueError("invalid Witchy TUF catalog release")
        name, release, target, length = (part.decode("ascii") for part in matched.groups())
        coordinate = (name, tuple(map(int, release.split(".")))) if VERSION.fullmatch(release) else None
        if coordinate is None or coordinate in entries or not NAME.fullmatch(name) or int(length) > (2 << 20):
            raise ValueError("invalid Witchy TUF catalog release")
        entries[coordinate] = {"name": name, "version": release, "target": target, "length": int(length)}
    persist_history(source, cache, version, root, latest_pin, timestamp, snapshot)
    return {"registry": registry, "snapshot": snapshot, "snapshot_version": snapshot_version,
            "timestamp": timestamp, "releases": [entries[key] for key in sorted(entries)]}


def persist_history(source, cache, version, initial_root, root_pin, timestamp_pin, snapshot_pin):
    root = initial_root if version == 1 else read_file(source / "root_history" / f"{version}.root.json", 512000)
    timestamp = read_file(source / "timestamp.json", TIMESTAMP_LIMIT)
    snapshot = read_file(source / "snapshot.json", 2 << 20)
    if digest_of(root) != root_pin or digest_of(timestamp) != timestamp_pin or digest_of(snapshot) != snapshot_pin:
        raise ValueError("cached TUF metadata changed after verification")
    wire = f"{version}\n{root_pin}\n{len(timestamp)}\n".encode("ascii") + timestamp + snapshot
    persist_if_changed(cache, HISTORY, wire, HISTORY_BYTES)


def persist_timestamp(source, cache, root_version, pin):
    wire = read_file(source / "timestamp.json", TIMESTAMP_LIMIT)
    if digest_of(wire) != pin:
        raise ValueError("cached TUF metadata changed after verification")
    checkpoint = f"{root_version}\n".encode("ascii") + wire
    persist_if_changed(cache, TIMESTAMP_FLOOR, checkpoint, TIMESTAMP_FLOOR_BYTES)


def persist_if_changed(parent, name, wire, limit):
    try:
        if read_file(name, limit, dir_fd=parent) == wire:
            return
    except FileNotFoundError:
        pass
    write_cache(parent, name, wire)


def write_cache(parent, name, wire):
    temporary = ".coven-history-" + secrets.token_hex(16)
    fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC,
                 0o600, dir_fd=parent)
    try:
        with os.fdopen(fd, "wb") as output:
            output.write(wire)
            output.flush()
            os.fsync(output.fileno())
        os.replace(temporary, name, src_dir_fd=parent, dst_dir_fd=parent)
        os.fsync(parent)
    finally:
        try:
            os.unlink(temporary, dir_fd=parent)
        except FileNotFoundError:
            pass
