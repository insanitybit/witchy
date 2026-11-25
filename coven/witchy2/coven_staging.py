"""Private candidate and approval storage, separate from the served registry."""
import contextlib
import json
import os
from pathlib import Path
import sqlite3
import stat

from coven_permissions import extended_acl
from witchy_release import parse_payload

DATABASE = "coven-staging.sqlite3"


def canonical(value):
    return json.dumps(value, ensure_ascii=True, sort_keys=True, separators=(",", ":"), allow_nan=False).encode("ascii")


def read_json(data):
    def unique(pairs):
        result = dict(pairs)
        if len(result) != len(pairs):
            raise ValueError("duplicate JSON field")
        return result

    try:
        return json.loads(data, object_pairs_hook=unique)
    except RecursionError:
        raise ValueError("invalid nested JSON") from None


def read_file(path, limit, *, owner_writes_only=False, dir_fd=None):
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=dir_fd)
    with os.fdopen(fd, "rb") as source:
        status = os.fstat(fd)
        if not stat.S_ISREG(status.st_mode):
            raise ValueError("expected a regular input file")
        if owner_writes_only and (status.st_uid != os.getuid() or status.st_mode & 0o022
                                  or status.st_nlink != 1 or extended_acl(fd)):
            raise ValueError("published file is shared or writable by another account")
        value = source.read(limit + 1)
    if len(value) > limit:
        raise ValueError(f"input exceeds {limit} bytes")
    return value


def require_private_directory(root):
    fd = os.open(root, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        mode = os.fstat(fd)
        if mode.st_uid != os.getuid() or mode.st_mode & 0o077 or extended_acl(fd):
            raise ValueError("Coven staging requires an owner-only directory")
    finally:
        os.close(fd)


def database_path(directory):
    root = Path(directory)
    require_private_directory(root)
    path = root / DATABASE
    try:
        info = path.lstat()
    except FileNotFoundError:
        raise ValueError("initialize the staging database with coven release init") from None
    if (not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid()
            or info.st_nlink != 1 or info.st_mode & 0o022):
        raise ValueError("invalid staging database")
    return root.resolve(strict=True) / DATABASE


def has_ci_provenance(directory):
    path = database_path(directory)
    try:
        with contextlib.closing(sqlite3.connect(path.as_uri() + "?mode=ro", uri=True, timeout=5)) as db:
            return db.execute(
                "SELECT 1 FROM sqlite_schema WHERE type='table' AND name='ci_receipts'").fetchone() is not None
    except sqlite3.Error as error:
        raise ValueError("Coven staging database operation failed") from error


@contextlib.contextmanager
def transaction(directory):
    path = database_path(directory)
    try:
        with contextlib.closing(sqlite3.connect(path.as_uri() + "?mode=rw", uri=True, timeout=5)) as db:
            with db:
                db.execute("BEGIN IMMEDIATE")
                yield db
    except sqlite3.Error as error:
        raise ValueError("Coven staging database operation failed") from error


def candidate(db, digest):
    row = db.execute("SELECT name, version, payload FROM candidates WHERE digest = ?", (digest,)).fetchone()
    if row is None:
        raise ValueError("unknown staged candidate")
    name, version, payload = row
    release = parse_payload(payload, name, version)
    if release.digest != digest:
        raise ValueError("staged bytes do not match their digest")
    return release
