"""Shuttle the metadata objects requested by the Witchy TUF verifier."""
from contextlib import contextmanager
from pathlib import Path
import os
import re
import secrets
import tempfile

from coven_https_client import DownloadRefused
from coven_staging import read_file
from coven_tuf_select import (
    CATALOG_MAX_ROLES, CATALOG_METADATA_BYTES, HISTORY, HISTORY_BYTES, PIN_LINE,
    TIMESTAMP_FLOOR, TIMESTAMP_FLOOR_BYTES, TIMESTAMP_LIMIT, finish_catalog, finish_selection, latest_root,
    persist_history, persist_timestamp, write_cache,
)
from witchy_release import digest_of

ROOT_LIMIT = 512000
ROOT_REPLY = re.compile(rb"root\n" + PIN_LINE)
TIMESTAMP = re.compile(rb"timestamp\n" + PIN_LINE)
REFERENCE = rb"([1-9][0-9]{0,15})\n([1-9][0-9]{0,6})\n"
SNAPSHOT = re.compile(rb"snapshot\n" + REFERENCE + PIN_LINE * 3)
TARGETS = re.compile(rb"targets\n" + REFERENCE + PIN_LINE * 4)
DELEGATED = re.compile(rb"role\n([a-z][a-z0-9-]{0,63})\n" + REFERENCE + PIN_LINE * 2)
ROLE_ENTRY = re.compile(rb"([a-z][a-z0-9-]{0,63})\n" + REFERENCE + PIN_LINE)


def refresh(run, cache, root, request, transport):
    catalog = request["coordinate"] is None
    with tempfile.TemporaryDirectory(prefix="coven-tuf-refresh-") as directory:
        source = Path(directory).resolve()
        (source / "root_history").mkdir(mode=0o700)
        recorded_root = latest_root(cache)
        previous = recorded_root or 1
        if previous > 1:
            with root_history(cache) as history:
                for version in range(2, previous + 1):
                    name = f"{version}.root.json"
                    wire = read_file(name, ROOT_LIMIT, dir_fd=history)
                    (source / "root_history" / name).write_bytes(wire)
        for name, limit in ((HISTORY, HISTORY_BYTES), (TIMESTAMP_FLOOR, TIMESTAMP_FLOOR_BYTES)):
            try:
                checkpoint = read_file(name, limit, dir_fd=cache)
            except FileNotFoundError:
                continue
            (source / name).write_bytes(checkpoint)
        latest = rotate_roots(transport, run, source, cache, root, previous)
        if latest == 1 and recorded_root is None:
            install_root(cache, root, 1, root)
        downloaded = []
        for _ in range(5):
            reply = run(source, latest, "refresh")
            if catalog and reply.startswith(b"catalog\n"):
                result = finish_catalog(reply, source, cache, latest, root, request["registry"])
                break
            if not catalog and reply.startswith((b"verified\n", b"absent\n")):
                result = finish_selection(reply, source, cache, latest, root)
                break
            if catalog and reply.startswith(b"roles\n"):
                downloaded.extend(fetch_catalog_roles(transport, source, cache, reply))
                continue
            name = fetch_metadata(transport, source, cache, reply, latest, root)
            if name is not None:
                downloaded.append(name)
        else:
            raise ValueError("Witchy TUF metadata refresh did not finish")
        for name in downloaded:
            write_cache(cache, f"verified-{name}", read_file(source / name, 2 << 20))
        return result


def verified_root_pin(reply):
    match = ROOT_REPLY.fullmatch(reply)
    if match is None:
        raise ValueError("Witchy TUF root verification refused")
    return match[1].decode("ascii")


def rotate_roots(transport, run, source, cache, initial, previous):
    verified_root_pin(run(source, previous, "root"))
    latest = previous
    for version in range(previous + 1, min(previous + 257, 2**53)):
        name = f"{version}.root.json"
        destination = source / "root_history" / name
        try:
            transport.fetch(f"/metadata/{name}", destination, max_length=ROOT_LIMIT)
        except DownloadRefused as error:
            if error.status not in (403, 404):
                raise
            break
        pin = verified_root_pin(run(source, version, "root"))
        wire = read_file(destination, ROOT_LIMIT)
        if digest_of(wire) != pin:
            raise ValueError("TUF root changed after verification")
        # Advance the durable root before requesting its successor.
        install_root(cache, initial, version, wire)
        latest = version
    return latest


@contextmanager
def root_history(cache, *, create=False):
    if create:
        try:
            os.mkdir("root_history", 0o700, dir_fd=cache)
        except FileExistsError:
            pass
    history = os.open("root_history", os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC, dir_fd=cache)
    try:
        yield history
    finally:
        os.close(history)


def install_root(cache, initial, version, wire):
    name = f"{version}.root.json"
    with root_history(cache, create=True) as history:
        if version > 1:
            try:
                os.stat("1.root.json", dir_fd=history, follow_symlinks=False)
            except FileNotFoundError:
                write_cache(history, "1.root.json", initial)
        write_cache(history, name, wire)
    link = ".coven-root-" + secrets.token_hex(16)
    os.symlink(f"root_history/{name}", link, dir_fd=cache)
    try:
        os.replace(link, "root.json", src_dir_fd=cache, dst_dir_fd=cache)
        os.fsync(cache)
    finally:
        try:
            os.unlink(link, dir_fd=cache)
        except FileNotFoundError:
            pass


def fetch_metadata(transport, source, cache, reply, root_version, root):
    if TIMESTAMP.fullmatch(reply):
        destination = source / "timestamp.json"
        if destination.exists():
            raise ValueError("Witchy TUF metadata refresh repeated a request")
        transport.fetch("/metadata/timestamp.json", destination, max_length=TIMESTAMP_LIMIT)
        return None
    if match := SNAPSHOT.fullmatch(reply):
        version, size, digest, timestamp, _ = (field.decode("ascii") for field in match.groups())
        persist_timestamp(source, cache, root_version, timestamp)
        role = "snapshot"
    elif match := TARGETS.fullmatch(reply):
        version, size, digest, timestamp, snapshot, root_pin = (field.decode("ascii") for field in match.groups())
        persist_history(source, cache, root_version, root, root_pin, timestamp, snapshot)
        role = "targets"
    elif match := DELEGATED.fullmatch(reply):
        role, version, size, digest, _ = (field.decode("ascii") for field in match.groups())
    else:
        raise ValueError("invalid Witchy TUF metadata request")
    return fetch_reference(transport, source, cache, role, version, int(size), digest)


def fetch_catalog_roles(transport, source, cache, reply):
    prefix, separator, tail = reply[6:-1].rpartition(b"\n")
    if not separator or not reply.endswith(b"\n") or not re.fullmatch(PIN_LINE, tail + b"\n"):
        raise ValueError("invalid Witchy TUF catalog role request")
    pending = prefix + b"\n"
    entries = []
    names = set()
    total = 0
    offset = 0
    while offset < len(pending):
        matched = ROLE_ENTRY.match(pending, offset)
        if matched is None:
            raise ValueError("invalid Witchy TUF catalog role request")
        role, version, size, digest = (part.decode("ascii") for part in matched.groups())
        length = int(size)
        total += length
        if role in names or role in {"root", "targets", "snapshot", "timestamp"} or length > (2 << 20):
            raise ValueError("invalid Witchy TUF catalog role request")
        names.add(role)
        entries.append((role, version, length, digest))
        offset = matched.end()
    if len(entries) > CATALOG_MAX_ROLES or total > CATALOG_METADATA_BYTES:
        raise ValueError("Witchy TUF catalog metadata exceeds its limits")
    for entry in entries:
        name = fetch_reference(transport, source, cache, *entry)
        if name is not None:
            yield name


def fetch_reference(transport, source, cache, role, version, length, digest):
    name = f"{role}.json"
    destination = source / name
    if destination.exists():
        raise ValueError("Witchy TUF metadata refresh repeated a request")
    wire = cached_metadata(cache, f"verified-{name}", length, digest)
    if wire is not None:
        destination.write_bytes(wire)
        return None
    transport.fetch(f"/metadata/{version}.{name}", destination, length=length)
    return name


def cached_metadata(parent, name, length, digest):
    try:
        wire = read_file(name, length, dir_fd=parent)
    except (OSError, ValueError):
        return None
    if len(wire) == length and digest_of(wire) == digest:
        return wire
    return None
