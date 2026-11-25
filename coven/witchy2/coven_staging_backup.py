"""Snapshot or recover the private staging store using SQLite's backup API."""
from contextlib import closing, contextmanager
from hashlib import sha384
import os
from pathlib import Path
import shutil
import sqlite3
import stat
import tempfile
import time

from coven_permissions import extended_acl
from coven_review_baseline import review_history
from coven_staging import DATABASE, read_file, require_private_directory
from coven_tuf_select import HISTORY_BYTES
from witchy_release import digest_of

MANIFEST = "manifest.json"
HISTORY = "history"
DATABASE_COPY_SECONDS = 30


@contextmanager
def input_file(path):
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, "rb") as source:
        info = os.fstat(fd)
        if (not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid()
                or info.st_nlink != 1 or info.st_mode & 0o022 or extended_acl(fd)):
            raise ValueError("invalid private backup input")
        yield source


def sync_directory(path):
    fd = os.open(path, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def write_private(path, data):
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, "wb") as target:
        target.write(data)
        target.flush()
        os.fsync(target.fileno())


@contextmanager
def new_directory(destination):
    """Publish a fully written directory, refusing any earlier destination."""
    if os.path.lexists(destination):
        raise ValueError("backup or recovery destination already exists")
    with tempfile.TemporaryDirectory(prefix=".coven-recovery-", dir=destination.parent) as directory:
        staging = Path(directory) / "complete"
        staging.mkdir(mode=0o700)
        yield staging
        sync_directory(staging)
        destination.mkdir(mode=0o700)
        try:
            os.replace(staging, destination)
        except OSError:
            destination.rmdir()
            raise
        sync_directory(destination.parent)


def read_database(path):
    return sqlite3.connect(path.as_uri() + "?mode=ro", uri=True, timeout=5)


def snapshot_database(source_path, target_path):
    write_private(target_path, b"")

    def progress(status, _remaining, _total):
        nonlocal last_progress
        now = time.monotonic()
        if now >= deadline:
            raise ValueError("staging database backup exceeded its time limit")
        if status in (sqlite3.SQLITE_OK, sqlite3.SQLITE_DONE):
            last_progress = now
        elif now - last_progress > 5:
            raise ValueError("staging database was busy during backup")

    with closing(read_database(source_path)) as source:
        with closing(sqlite3.connect(target_path, timeout=5)) as target:
            last_progress = time.monotonic()
            deadline = last_progress + DATABASE_COPY_SECONDS
            source.backup(target, pages=256, progress=progress, sleep=0.02)


def seal_database(path, workers):
    with closing(sqlite3.connect(path, timeout=5)) as db:
        if db.execute("PRAGMA journal_mode=DELETE").fetchone() != ("delete",):
            raise ValueError("staging backup database could not be made standalone")
        check_integrity(db)
    workers.check_database(path)
    with input_file(path) as data:
        os.fsync(data.fileno())
        return "sha384:" + sha384_stream(data)


def sha384_stream(source):
    digest = sha384()
    while data := source.read(1 << 20):
        digest.update(data)
    return digest.hexdigest()


def check_integrity(db):
    db.execute("PRAGMA trusted_schema=OFF")
    if db.execute("PRAGMA integrity_check(1)").fetchone() != ("ok",):
        raise ValueError("invalid Coven staging backup database")
    if db.execute("PRAGMA foreign_key_check").fetchone() is not None:
        raise ValueError("broken references in Coven staging backup database")


def sibling_paths(source, destination):
    require_private_directory(source)
    destination = destination.absolute()
    if destination.resolve().is_relative_to(source.resolve()):
        raise ValueError("backup and staging directories must be separate")
    return source.resolve(strict=True), destination


def backup(directory, out, workers):
    source, destination = sibling_paths(directory, out)
    try:
        with input_file(source / DATABASE), new_directory(destination) as staging:
            with review_history(source) as (_, history):
                snapshot_database(source / DATABASE, staging / DATABASE)
            digest = seal_database(staging / DATABASE, workers)
            if history:
                write_private(staging / HISTORY, history)
            pin, manifest = workers.create_manifest(digest, digest_of(history) if history else None)
            write_private(staging / MANIFEST, manifest)
    except sqlite3.Error:
        raise ValueError("could not back up the Coven staging database") from None
    return {"pin": pin}


def verify_manifest(source, expected, workers):
    wire = read_file(source / MANIFEST, 1024, owner_writes_only=True)
    manifest = workers.verify_manifest(wire, expected)
    expected_files = {MANIFEST, DATABASE}
    if manifest["review_history"] is not None:
        expected_files.add(HISTORY)
    if {path.name for path in source.iterdir()} != expected_files:
        raise ValueError("unexpected files in the Coven staging backup")
    return manifest


def restore(backup_dir, expected, directory, workers):
    source, destination = sibling_paths(backup_dir, directory)
    manifest = verify_manifest(source, expected, workers)
    try:
        with new_directory(destination) as staging:
            target = staging / DATABASE
            write_private(target, b"")
            with input_file(source / DATABASE) as original, target.open("wb") as recovered:
                shutil.copyfileobj(original, recovered, length=1 << 20)
            with input_file(target) as recovered:
                if "sha384:" + sha384_stream(recovered) != manifest["database"]:
                    raise ValueError("staging backup database differs from its manifest")
            with closing(read_database(target)) as db:
                check_integrity(db)
            workers.check_database(target, clear_pending=True)
            with input_file(target) as recovered:
                os.fsync(recovered.fileno())
            if manifest["review_history"] is not None:
                history = read_file(source / HISTORY, HISTORY_BYTES, owner_writes_only=True)
                if digest_of(history) != manifest["review_history"]:
                    raise ValueError("review baseline checkpoint differs from its backup manifest")
                checkpoint = staging / ".review-baseline"
                checkpoint.mkdir(mode=0o700)
                write_private(checkpoint / HISTORY, history)
                sync_directory(checkpoint)
    except sqlite3.Error:
        raise ValueError("could not restore the Coven staging database") from None
