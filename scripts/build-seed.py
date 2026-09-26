#!/usr/bin/env python3
"""Preserve content-identical source mtimes across merge-queue rebases.

Cargo 1.100 and mbx-managed targets replaced Witchy's cross-worktree target
seed. The coordinator still snapshots source mtimes before replaying a queued
candidate so an amended retry does not recompile files whose blobs are
unchanged.

    build-seed.py snapshot-mtimes <checkout> <snapshot>
    build-seed.py restore-mtimes <checkout> <snapshot>

The historical filename remains because the merge coordinator invokes it.
"""

import hashlib
import json
import os
import subprocess
import sys


def tracked(checkout):
    """{path: blob id} of the index, which matches a clean checkout."""
    out = subprocess.run(["git", "-C", checkout, "ls-files", "-s", "-z"],
                         check=True, capture_output=True).stdout
    files = {}
    for entry in out.split(b"\0"):
        if not entry:
            continue
        meta, path = entry.split(b"\t", 1)
        files[os.fsdecode(path)] = meta.split()[1].decode()
    return files


def modified(checkout):
    out = subprocess.run(["git", "-C", checkout, "status", "--porcelain", "-z",
                          "--untracked-files=no"],
                         check=True, capture_output=True).stdout
    return {os.fsdecode(entry[3:]) for entry in out.split(b"\0") if entry}


def directories(files):
    """{dir: digest of its tracked direct children} for every ancestor dir."""
    children = {}
    for path in files:
        parts = path.split("/")
        for depth in range(len(parts) - 1):
            parent = "/".join(parts[:depth + 1])
            grand = "/".join(parts[:depth])
            children.setdefault(grand, set()).add(parent)
        children.setdefault("/".join(parts[:-1]), set()).add(path)
    return {d: hashlib.blake2b("\0".join(sorted(c)).encode(), digest_size=16).hexdigest()
            for d, c in children.items() if d}


def snapshot_mtimes(checkout, snapshot):
    """Remember a clean candidate's tracked mtimes before Git replaces it.

    The merge coordinator prepares every attempt by checking the gate worktree
    back out at master and replaying the submitted patches. Git consequently
    gives every file in a broad patch a new mtime, even when an amended retry
    changed only one test. Cargo then recompiles every crate touched by the
    original patch. The blob id makes restoring an old mtime content-safe: a
    file that actually changed keeps Git's new mtime and rebuilds normally.
    """
    checkout = os.path.realpath(checkout)
    dirty = modified(checkout)
    if dirty:
        print(f"build-seed: {checkout} has {len(dirty)} modified file(s); "
              "not snapshotting candidate mtimes", file=sys.stderr)
        return 1
    files = tracked(checkout)
    state = {"files": {}, "dirs": {}}
    for path, blob in files.items():
        try:
            state["files"][path] = [blob, os.lstat(os.path.join(checkout, path)).st_mtime_ns]
        except FileNotFoundError:
            pass
    for path, digest in directories(files).items():
        try:
            state["dirs"][path] = [digest, os.stat(os.path.join(checkout, path)).st_mtime_ns]
        except FileNotFoundError:
            pass
    tmp = f"{snapshot}.{os.getpid()}.tmp"
    try:
        with open(tmp, "w") as out:
            json.dump(state, out)
        os.replace(tmp, snapshot)
    finally:
        try:
            os.unlink(tmp)
        except FileNotFoundError:
            pass
    return 0


def restore_mtimes(checkout, snapshot):
    """Restore mtimes only where the prepared candidate has the same blob."""
    checkout = os.path.realpath(checkout)
    dirty = modified(checkout)
    if dirty:
        print(f"build-seed: {checkout} has {len(dirty)} modified file(s); "
              "not restoring candidate mtimes", file=sys.stderr)
        return 1
    with open(snapshot) as source:
        state = json.load(source)
    files = tracked(checkout)
    restored_files = 0
    for path, (blob, mtime_ns) in state.get("files", {}).items():
        if files.get(path) != blob:
            continue
        try:
            os.utime(os.path.join(checkout, path), ns=(mtime_ns, mtime_ns),
                     follow_symlinks=False)
            restored_files += 1
        except FileNotFoundError:
            pass
    current_dirs = directories(files)
    restored_dirs = 0
    # Files first, directories last: restoring a child's mtime does not touch
    # its parent, while any future metadata work must not perturb a directory
    # that a build script watches through rerun-if-changed=<dir>.
    for path, (digest, mtime_ns) in state.get("dirs", {}).items():
        if current_dirs.get(path) != digest:
            continue
        try:
            os.utime(os.path.join(checkout, path), ns=(mtime_ns, mtime_ns))
            restored_dirs += 1
        except FileNotFoundError:
            pass
    print(f"build-seed: preserved mtimes for {restored_files} unchanged tracked file(s) "
          f"and {restored_dirs} director{'y' if restored_dirs == 1 else 'ies'}")
    return 0


if __name__ == "__main__":
    if len(sys.argv) == 4 and sys.argv[1] == "snapshot-mtimes":
        sys.exit(snapshot_mtimes(*sys.argv[2:]))
    if len(sys.argv) == 4 and sys.argv[1] == "restore-mtimes":
        sys.exit(restore_mtimes(*sys.argv[2:]))
    print(__doc__, file=sys.stderr)
    sys.exit(2)
