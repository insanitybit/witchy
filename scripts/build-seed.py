#!/usr/bin/env python3
"""Publish and apply the fresh-worktree build seed.

A new worktree used to rebuild every workspace crate cold even after its
target/ was cloned from a warm one, for two reasons measured 2026-09-23:

  * `git worktree add` stamps every checked-out file with the current time.
    Cargo judges a path unit by comparing its sources' mtimes to the
    fingerprint's build time, so every workspace crate looked edited
    ("the file `crates/witchy-cap-model/src/authority.rs` has changed").
  * The clone source, the main checkout's target/, is not built by anyone:
    agents never build in the main checkout, so it was 2-3 days behind master.

The merge gate builds exactly the landed master tip. `publish` (run by the
coordinator under the gate lock right after a green merge; not after an idle
prewarm, whose other target generation would need a full, uninterruptible
copy under the lock) snapshots that target, with the same filtering worktree-warm uses,
together with a manifest of each tracked file's blob id and mtime in the gate
checkout. `apply` (run by worktree-warm) makes the seeded target describe the
new worktree exactly as it described the gate checkout (dep-info first, so an
interrupted apply leaves every source at its checkout time):

  * each file whose blob id matches the manifest gets the gate checkout's
    mtime, so a crate whose sources are identical to what the gate built is
    Fresh; every other tracked, modified or untracked file is stamped with
    the current time, later than anything in the seed, so it rebuilds however
    the checkout and the seed's publication were ordered. Directory mtimes are copied only when the directory's tracked
    entries are identical, because a build script's `rerun-if-changed=<dir>`
    (witchy-wir's `src`) reads them.
  * cargo records a source outside the crate's own directory by ABSOLUTE path
    in the fingerprint's dep-info (witchy-syntax and witchy-lower embed
    `std/*.witchy` through `../../../std`). Left alone, those entries would
    keep pointing at the gate checkout, and an edit to the worktree's
    std/*.witchy would never rebuild the compiler. `apply` rewrites the gate
    checkout prefix to the worktree. A unit whose dep-info it cannot decode
    exactly, or that would still name an absolute path outside the worktree,
    the cargo home and the toolchain, loses its fingerprint and rebuilds.
  * Rust test code can embed Cargo's checkout-specific CARGO_MANIFEST_DIR
    without recording it as an environment dependency. `publish` records the
    test units whose tracked sources use that compile-time macro; `apply`
    drops only those fingerprints so a test can never read the gate checkout.

    build-seed.py publish <gate-checkout> <gate-target> <seed-root>  # locked
    build-seed.py finish <seed-root>                                 # unlocked
    build-seed.py apply <seed-dir> <worktree> [<target-dir>]
    build-seed.py refresh <seed-dir> <worktree> [<target-dir>]  # scripts/bin/cargo
    build-seed.py snapshot-mtimes <checkout> <snapshot>
    build-seed.py restore-mtimes <checkout> <snapshot>
"""

import fcntl, hashlib, json, os, re, shutil, struct, subprocess, sys, time

MANIFEST = "manifest.json"
# A worktree-warm clone (about a minute) may still be reading a seed after it
# stops being current; any older seed only pins artifacts the gate replaced.
KEEP_OLD_SECONDS = 10 * 60
MANIFEST_DIR_MACRO = re.compile(rb'(?:option_)?env\s*!\s*\(\s*"CARGO_MANIFEST_DIR"')


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


try:
    import ctypes
    _clonefile = ctypes.CDLL("/usr/lib/libSystem.dylib").clonefile
except (OSError, AttributeError):
    _clonefile = None


def clone(src, dst):
    """Copy-on-write clone of one file (APFS clonefile, else a plain copy)."""
    os.makedirs(os.path.dirname(dst), exist_ok=True)
    if os.path.islink(src):
        os.symlink(os.readlink(src), dst)
    elif _clonefile is None or _clonefile(src.encode(), dst.encode(), 0) != 0:
        shutil.copy2(src, dst)


def target_files(target):
    """Relative paths worth seeding: what worktree-warm's clone_target keeps
    (no incremental/, deps/*.rcgu.o, tmp/ or the executable-keyed testcache)."""
    for top, dirs, names in os.walk(target):
        rel_top = os.path.relpath(top, target)
        if rel_top == ".":
            dirs[:] = [d for d in dirs if d not in ("tmp", "witchy-testcache")]
        dirs[:] = [d for d in dirs if d != "incremental"]
        links = [d for d in dirs if os.path.islink(os.path.join(top, d))]
        dirs[:] = [d for d in dirs if d not in links]
        in_deps = os.path.basename(top) == "deps"
        for name in names + links:
            if in_deps and name.endswith(".rcgu.o"):
                continue
            yield os.path.normpath(os.path.join(rel_top, name))


def load_manifest(seed):
    with open(os.path.join(seed, MANIFEST)) as f:
        return json.load(f)


def workspace_package_roots(checkout):
    """{package name: source root} without compiling the workspace."""
    metadata = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--no-deps"],
        cwd=checkout, check=True, capture_output=True, text=True,
    )
    return {
        package["name"]: os.path.dirname(os.path.realpath(package["manifest_path"]))
        for package in json.loads(metadata.stdout)["packages"]
    }


def path_bound_fingerprint_units(checkout, target, files):
    """Fingerprint units whose sources compile the publisher checkout path.

    Cargo does not record CARGO_MANIFEST_DIR as an env dependency because it
    supplies that value itself. Reusing such a unit in another worktree can
    therefore run an executable that still reads the gate checkout. Record
    only those units so apply can invalidate them without discarding the rest
    of the warm workspace build.
    """
    sensitive = set()
    for rel in files:
        if not rel.endswith(".rs"):
            continue
        path = os.path.join(checkout, rel)
        try:
            with open(path, "rb") as source:
                if MANIFEST_DIR_MACRO.search(source.read()):
                    sensitive.add(os.path.normpath(path))
        except FileNotFoundError:
            pass
    if not sensitive:
        return []

    packages = workspace_package_roots(checkout)
    package_names = sorted(packages, key=len, reverse=True)
    bound = []
    for top, dirs, _ in os.walk(target):
        if os.path.basename(top) != ".fingerprint":
            continue
        units, dirs[:] = list(dirs), []
        for unit in units:
            package = next(
                (name for name in package_names if unit.startswith(name + "-")),
                None,
            )
            if package is None:
                continue
            unit_dir = os.path.join(top, unit)
            # Non-test libraries can list cfg(test) source files in dep-info
            # without compiling their contents. Only executable test units
            # can observe the checkout-specific value at runtime.
            if not any(
                name.startswith("test-") and not name.endswith(".json")
                for name in os.listdir(unit_dir)
            ):
                continue
            is_bound = False
            for name in os.listdir(unit_dir):
                if not name.startswith("dep-"):
                    continue
                try:
                    with open(os.path.join(unit_dir, name), "rb") as dep:
                        decoded = decode_dep_info(dep.read())
                except FileNotFoundError:
                    continue
                if decoded is None:
                    continue
                for _, encoded in decoded[0]:
                    path = os.fsdecode(encoded)
                    if not os.path.isabs(path):
                        path = os.path.join(packages[package], path)
                    if os.path.normpath(path) in sensitive:
                        is_bound = True
                        break
                if is_bound:
                    break
            if is_bound:
                bound.append(os.path.relpath(unit_dir, target))
    return sorted(bound)


def publish(checkout, target, seed_root):
    """The part that must run under the gate lock: the manifest, and a clone
    of every target file the gate wrote since the previous seed's snapshot.
    A full clone is ~15k files, 60-70s at the usual load; the files one gate
    rewrites are a few hundred. `finish` fills in the rest from the previous
    seed after the lock is released."""
    checkout, target = os.path.realpath(checkout), os.path.realpath(target)
    files = tracked(checkout)
    dirty = modified(checkout)
    if dirty:
        # The gate checkout is reset for every attempt; anything else means
        # the target was not built from the index this manifest would record.
        print(f"build-seed: {checkout} has {len(dirty)} modified file(s); not publishing",
              file=sys.stderr)
        return 1
    snapshot_ns = time.time_ns()
    prev, prev_ns = None, 0
    try:
        prev = os.path.realpath(os.path.join(seed_root, "current"))
        previous = load_manifest(prev)
        # "Unchanged since the previous snapshot" only means "identical to
        # the previous seed's copy" when both came from the same directory;
        # the gate alternates target/ and target-prewarm/ generations.
        if previous["target"] != target:
            prev = None
        else:
            prev_ns = previous["snapshot_ns"]
    except (OSError, ValueError, KeyError):
        prev = None
    os.makedirs(seed_root, exist_ok=True)
    staging = os.path.join(seed_root, f".seed-{time.strftime('%Y%m%dT%H%M%S')}-{os.getpid()}")
    os.makedirs(os.path.join(staging, "target"))
    manifest = {"head": subprocess.run(["git", "-C", checkout, "rev-parse", "HEAD"],
                                       check=True, capture_output=True, text=True).stdout.strip(),
                "checkout": checkout, "target": target, "snapshot_ns": snapshot_ns,
                "fill_from": prev, "files": {}, "dirs": {}, "paths": [],
                "path_bound_units": path_bound_fingerprint_units(checkout, target, files)}
    for path, blob in files.items():
        try:
            manifest["files"][path] = [blob, os.lstat(os.path.join(checkout, path)).st_mtime_ns]
        except FileNotFoundError:
            pass
    for d, digest in directories(files).items():
        try:
            manifest["dirs"][d] = [digest, os.stat(os.path.join(checkout, d)).st_mtime_ns]
        except FileNotFoundError:
            pass
    copied = 0
    for rel in target_files(target):
        src = os.path.join(target, rel)
        try:
            stat = os.lstat(src)
            # ctime too: cargo's uplift hard-links debug/<bin> to a deps/
            # file that keeps an older mtime, and a copy can keep its source's.
            changed = max(stat.st_mtime_ns, stat.st_ctime_ns) > prev_ns
            if changed or prev is None or not os.path.lexists(os.path.join(prev, "target", rel)):
                clone(src, os.path.join(staging, "target", rel))
                copied += 1
            manifest["paths"].append(rel)
        except FileNotFoundError:
            pass
    with open(os.path.join(staging, MANIFEST), "w") as out:
        json.dump(manifest, out)
    print(f"build-seed: staged {copied} of {len(manifest['paths'])} file(s) of "
          f"{manifest['head'][:9]} with {len(manifest['path_bound_units'])} "
          f"path-bound test unit(s) at {staging}")
    return 0


def finish(seed_root):
    """Unlocked: complete every staged seed from its previous seed and make
    the newest one current."""
    lock = open(os.path.join(seed_root, ".finish.lock"), "w")
    fcntl.flock(lock, fcntl.LOCK_EX)  # two quick merges can both finish
    link = os.path.join(seed_root, "current")
    finished = None
    now = time.time()
    for name in sorted(os.listdir(seed_root)):
        staging = os.path.join(seed_root, name)
        if not name.startswith(".seed-"):
            continue
        try:
            manifest = load_manifest(staging)
        except (OSError, ValueError):
            # A publish killed before writing its manifest.
            if now - os.lstat(staging).st_mtime > KEEP_OLD_SECONDS:
                shutil.rmtree(staging, ignore_errors=True)
            continue
        prev = manifest["fill_from"]
        complete = True
        if prev:
            for rel in manifest["paths"]:  # never resurrect a deleted path
                dst = os.path.join(staging, "target", rel)
                if os.path.lexists(dst):
                    continue
                try:
                    clone(os.path.join(prev, "target", rel), dst)
                except FileNotFoundError:
                    complete = False
                    break
        if not complete:
            # The fill source was reclaimed; the next publish starts from
            # whatever seed is current then.
            print(f"build-seed: {prev} vanished; discarding {staging}", file=sys.stderr)
            shutil.rmtree(staging, ignore_errors=True)
            continue
        final = os.path.join(seed_root, name[1:])
        os.rename(staging, final)
        finished = final
    if finished is None:
        # The coordinator may pause for low disk after the last publication.
        # Prune expired predecessors even when there is no newly staged seed;
        # otherwise the next gate cannot start to trigger another finish.
        try:
            finished = os.path.realpath(link)
            load_manifest(finished)
        except (OSError, ValueError, TypeError, AttributeError):
            return 0
    # `publish` runs under the gate lock, but `finish` deliberately runs after
    # releasing it. A slow older finalizer can therefore acquire this lock
    # after a newer seed is already current. Never move the pointer backward:
    # directory names include wall time but snapshot_ns is the authoritative
    # publication order (and also handles two publishes in one second).
    try:
        current = os.path.realpath(link)
        current_snapshot = load_manifest(current).get("snapshot_ns", 0)
        finished_snapshot = load_manifest(finished).get("snapshot_ns", 0)
        if current_snapshot > finished_snapshot:
            finished = current
    except (OSError, ValueError, TypeError, AttributeError):
        pass
    try:
        os.utime(os.path.realpath(link))  # its KEEP_OLD_SECONDS window starts now
    except OSError:
        pass
    tmp_link = f"{link}.{os.getpid()}"
    os.symlink(os.path.basename(finished), tmp_link)
    os.replace(tmp_link, link)
    for name in os.listdir(seed_root):
        path = os.path.join(seed_root, name)
        if name.startswith("seed-") and path != finished \
                and time.time() - os.lstat(path).st_mtime > KEEP_OLD_SECONDS:
            shutil.rmtree(path, ignore_errors=True)
    print(f"build-seed: current is {finished}")
    return 0


# Cargo's encoded fingerprint dep-info (cargo 1.9x): a 6-byte header, then
# u32 file count, per file {u8 path type, u32-length path, u8 checksum flag},
# then u32 env count, per var {u32-length key, u8 has-value, value}. Only a
# file this decoder re-encodes to the identical bytes is ever rewritten.
DEP_INFO_HEADER = b"\x01\x00\x00\x00\xff\x01"


def decode_dep_info(data):
    if not data.startswith(DEP_INFO_HEADER):
        return None
    pos = len(DEP_INFO_HEADER)

    def u8():
        nonlocal pos
        pos += 1
        return data[pos - 1]

    def u32():
        nonlocal pos
        pos += 4
        return struct.unpack_from("<I", data, pos - 4)[0]

    def blob():
        nonlocal pos
        size = u32()
        pos += size
        if pos > len(data):
            raise ValueError("truncated")
        return data[pos - size:pos]

    files, env = [], []
    try:
        for _ in range(u32()):
            kind = u8()
            path = blob()
            if kind not in (0, 1) or u8() != 0:  # no checksum freshness
                return None
            files.append((kind, path))
        for _ in range(u32()):
            key, flag = blob(), u8()
            if flag not in (0, 1):
                return None
            env.append((key, blob() if flag else None))
    except (IndexError, ValueError, struct.error):
        return None
    return (files, env) if pos == len(data) else None


def encode_dep_info(files, env):
    out = bytearray(DEP_INFO_HEADER)
    out += struct.pack("<I", len(files))
    for kind, path in files:
        out += bytes([kind]) + struct.pack("<I", len(path)) + path + b"\0"
    out += struct.pack("<I", len(env))
    for key, value in env:
        out += struct.pack("<I", len(key)) + key
        out += b"\0" if value is None else b"\1" + struct.pack("<I", len(value)) + value
    return bytes(out)


def foreign_path(path, roots):
    """An absolute dep-info path outside every root this worktree may name."""
    return path.startswith(b"/") and not any(path.startswith(root) for root in roots)


def allowed_roots(new_root):
    """Absolute path prefixes a worktree's dep-info may name: the worktree,
    the cargo home, the toolchain and the system."""
    cargo_home = os.environ.get("CARGO_HOME") or os.path.expanduser("~/.cargo")
    sysroot = subprocess.run(["rustc", "--print", "sysroot"], capture_output=True,
                             text=True).stdout.strip()
    allowed = [os.fsencode(new_root.rstrip("/")) + b"/"]
    allowed += [os.fsencode(os.path.realpath(p).rstrip("/")) + b"/"
                for p in (cargo_home, sysroot) if p]
    allowed += [b"/opt/homebrew/", b"/usr/", b"/Library/", b"/System/", b"/Applications/"]
    # A dependency the gate restored through mbx reads its build script's
    # generated sources from the shared, content-addressed store under the
    # main checkout's state/mbx/ (e.g. cranelift-codegen's out-dirs). Dropping
    # those units rebuilt cranelift and wasmtime in every seeded worktree.
    common = subprocess.run(["git", "-C", new_root, "rev-parse", "--path-format=absolute",
                             "--git-common-dir"], capture_output=True, text=True).stdout.strip()
    if common:
        mbx = os.path.join(os.path.dirname(os.path.realpath(common)), "state", "mbx")
        allowed.append(os.fsencode(mbx) + b"/")
    return allowed


def rebase_unit(unit_dir, old, new, allowed):
    """Rebase one fingerprint unit's dep-info from prefix old to new (bytes,
    '/'-terminated). Drops the unit, and returns "dropped", when a dep-info
    file cannot be decoded exactly or would still name a foreign path;
    otherwise "rewritten" or "unchanged". Each file is replaced atomically."""
    result = "unchanged"
    for name in os.listdir(unit_dir):
        if not name.startswith("dep-"):
            continue
        path = os.path.join(unit_dir, name)
        with open(path, "rb") as f:
            data = f.read()
        decoded = decode_dep_info(data)
        if decoded is None or encode_dep_info(*decoded) != data:
            if b"/" in data:  # cannot vouch for what it names
                shutil.rmtree(unit_dir, ignore_errors=True)
                return "dropped"
            continue
        files = [(kind, new + p[len(old):] if p.startswith(old) else p)
                 for kind, p in decoded[0]]
        if any(foreign_path(p, allowed) for _, p in files):
            shutil.rmtree(unit_dir, ignore_errors=True)  # cargo rebuilds it
            return "dropped"
        if files == decoded[0]:
            continue
        stat = os.stat(path)
        tmp = f"{path}.seed-{os.getpid()}"
        with open(tmp, "wb") as f:
            f.write(encode_dep_info(files, decoded[1]))
        os.utime(tmp, ns=(stat.st_atime_ns, stat.st_mtime_ns))
        os.replace(tmp, path)
        result = "rewritten"
    return result


def rebase_dep_info(target, old_root, new_root):
    """Point absolute dep-info paths under old_root at new_root, and drop the
    fingerprint of any unit that would still name a file outside this
    worktree, the cargo home or the toolchain (e.g. the gate checkout spelled
    another way). Returns (rewritten, dropped) unit counts."""
    old = os.fsencode(old_root.rstrip("/")) + b"/"
    new = os.fsencode(new_root.rstrip("/")) + b"/"
    allowed = allowed_roots(new_root)
    rewritten = dropped = 0
    for top, dirs, _ in os.walk(target):
        dirs[:] = [d for d in dirs if d != "incremental"]
        if os.path.basename(top) != ".fingerprint":
            continue
        units, dirs[:] = list(dirs), []
        for unit in units:
            outcome = rebase_unit(os.path.join(top, unit), old, new, allowed)
            rewritten += outcome == "rewritten"
            dropped += outcome == "dropped"
    return rewritten, dropped


def drop_path_bound_units(target, units):
    """Invalidate checkout-bound units recorded by the seed publisher."""
    dropped = 0
    target = os.path.realpath(target)
    for rel in units:
        path = os.path.realpath(os.path.join(target, rel))
        if path == target or not path.startswith(target + os.sep):
            continue
        if os.path.basename(os.path.dirname(path)) != ".fingerprint":
            continue
        if os.path.isdir(path):
            shutil.rmtree(path)
            dropped += 1
    return dropped


def apply(seed, worktree, target_dir=None):
    """Rebase the seeded dep-info onto this worktree, THEN give identical
    files the gate's mtimes. The order is the safety property: until every
    unit's dep-info names this worktree, sources keep their checkout time and
    cargo rebuilds; an interrupted apply only costs rebuilds."""
    manifest = load_manifest(seed)
    worktree = os.path.realpath(worktree)
    target = target_dir or os.path.join(worktree, "target")
    path_dropped = drop_path_bound_units(target, manifest.get("path_bound_units", []))
    rewritten, dropped = rebase_dep_info(target, manifest["checkout"], worktree)
    files = tracked(worktree)
    dirty = modified(worktree)
    # Everything that does NOT match the gate checkout is stamped now, which
    # is later than any fingerprint in the seed: a file checked out or edited
    # before the seed was published must still read as changed.
    now = time.time_ns()

    def stamp(rel, ns):
        try:
            os.utime(os.path.join(worktree, rel), ns=(ns, ns), follow_symlinks=False)
        except FileNotFoundError:
            pass  # deleted in the working tree

    matched = 0
    for path, blob in files.items():
        entry = manifest["files"].get(path)
        if entry and entry[0] == blob and path not in dirty:
            stamp(path, entry[1])
            matched += 1
        else:
            stamp(path, now)
    untracked = subprocess.run(["git", "-C", worktree, "ls-files", "-z", "--others",
                                "--exclude-standard"], check=True, capture_output=True).stdout
    for entry in untracked.split(b"\0"):
        # Never the seeded build caches themselves (target/, target-<agent>/),
        # even where no .gitignore hides them: that would restamp every
        # artifact "now" and make it newer than an edited source.
        if entry and not entry.split(b"/", 1)[0].startswith(b"target"):
            stamp(os.fsdecode(entry), now)
    for d, digest in directories(files).items():
        entry = manifest["dirs"].get(d)
        stamp(d, entry[1] if entry and entry[0] == digest else now)
    print(f"build-seed: {matched}/{len(files)} files match seed {manifest['head'][:9]}; "
          f"{rewritten} dep-info file(s) rebased onto this worktree, "
          f"{dropped} unsafe and {path_dropped} path-bound unit(s) dropped")
    return 0


UNIT_DIR = re.compile(r"^(.+)-([0-9a-f]{16})$")
REFRESH_STAMP = ".witchy-seed-refresh"
# Files whose change alters every unit's identity or the build environment;
# a worktree that differs from the seed in any of them is not refreshed.
GLOBAL_INPUTS = ("Cargo.toml", "Cargo.lock", ".cargo/config.toml",
                 "rust-toolchain", "rust-toolchain.toml")


def fingerprint_units(debug):
    """{unit dir name: {"files": [...], "hashes": {hex}, "json": [...]}}.
    A unit's hash file (lib-<crate>, test-lib-<crate>, ...) holds its
    fingerprint hash as little-endian hex; dependents name it in their json."""
    units = {}
    root = os.path.join(debug, ".fingerprint")
    try:
        names = os.listdir(root)
    except FileNotFoundError:
        return units
    for unit in names:
        unit_dir = os.path.join(root, unit)
        try:
            files = os.listdir(unit_dir)
        except (NotADirectoryError, FileNotFoundError):
            continue
        info = {"files": files, "hashes": set(), "json": []}
        for name in files:
            path = os.path.join(unit_dir, name)
            if name.endswith(".json"):
                try:
                    with open(path) as f:
                        info["json"].append(json.load(f))
                except (OSError, ValueError):
                    info["json"].append(None)
            elif not name.startswith(("dep-", "output-")) and name != "invoked.timestamp" \
                    and ".seed-" not in name:
                try:
                    with open(path) as f:
                        info["hashes"].add(f.read().strip())
                except (OSError, UnicodeDecodeError):
                    pass
        units[unit] = info
    return units


def dep_hashes(info):
    """Fingerprint hashes of a unit's dependencies, in hash-file form, or
    None when a fingerprint cannot be read."""
    hashes = set()
    for fp in info["json"]:
        if not isinstance(fp, dict):
            return None
        for dep in fp.get("deps", []):
            try:
                hashes.add(struct.pack("<Q", int(dep[-1])).hex())
            except (TypeError, ValueError, IndexError, struct.error):
                return None
    return hashes


def index_deps(debug):
    """{unit hash: [deps/ file names carrying it]}"""
    index = {}
    try:
        names = os.listdir(os.path.join(debug, "deps"))
    except FileNotFoundError:
        return index
    for name in names:
        match = re.search(r"-([0-9a-f]{16})(?:\.|$)", name)
        if match and not name.endswith(".rcgu.o"):
            index.setdefault(match.group(1), []).append(name)
    return index


def unit_outputs(debug, unit, deps_index):
    """Profile-relative outputs of one unit: deps/*-<hash>[.*] and build/<unit>/**."""
    outputs = [os.path.join("deps", n) for n in deps_index.get(unit.rsplit("-", 1)[1], [])]
    build = os.path.join(debug, "build", unit)
    if os.path.isdir(build):
        for top, _, names in os.walk(build):
            outputs += [os.path.relpath(os.path.join(top, n), debug) for n in names]
    return outputs


def write_stamp(path, key):
    try:
        with open(path, "w") as f:
            f.write(key)
    except OSError:
        pass
    return 0


def refresh(seed, worktree, target_dir=None):
    """Bring a seeded target's unedited workspace units up to the CURRENT seed.

    A worktree is seeded once, from the master of its creation, and a pool
    worktree is recycled with whatever its previous task built. After master
    moves (a rebase, a pool claim), every workspace crate that changed
    between the two masters used to recompile in every worktree, although the
    gate had already built exactly those sources and the seed holds the
    result. This replaces such a unit with the seed's copy when it is
    provably the unit cargo would build here:

      * every file its dep-info (or rerun-if-changed path) names in the
        checkout hashes, as it is on disk now, to the manifest's blob, and no
        untracked file sits under a rerun-if-changed path;
      * it was fresh against those sources in the gate (no named source is
        newer than its dep-info), so a stale leftover unit is never revived;
      * every workspace unit it depends on qualifies too, and every
        third-party unit it depends on is present here with the same hash.

    Source mtimes are NEVER touched: other target directories of the same
    worktree judge freshness by them. Instead each copied unit (fingerprint
    and outputs) is stamped with an instant taken before the hashing, and
    every file it reads must be older than that instant, so an edit made at
    any later time is newer than the stamp and rebuilds it. Cargo calls a
    unit stale when a source is newer than its dep-info, or a dependency's
    output newer than its own; stamping every refreshed unit with the same
    instant satisfies both, and any unit here the refresh did not stamp
    (an edited crate, its dependents) sees newer dependencies and rebuilds.
    A unit that already is the seed's and fresh here is left alone, so its
    dependents are not disturbed. Units are swapped fingerprint-first: an
    interrupted refresh costs rebuilds, never a stale answer. The target's
    cargo lock is held throughout, and a busy target is skipped.
    Third-party units this target lacks entirely are copied too.

    It only serves identities the gate built: a feature set or profile the
    gate never compiled (e.g. the shim's lowerer-only selection) still
    compiles here. Measured 2026-09-25, `nextest run -p witchy --no-run` in
    a target last built at a master two landings behind the seed (syntax,
    types, interp changed), HEAD at the seed's master: 282 CPU-s / 150 s wall
    before (nine workspace crates compiled), 49 CPU-s / 30 s after (only the
    root crate, whose units the gate had not built fresh against these
    sources), load 20-26; the refresh took 1.8 s. WITCHY_SEED_REFRESH_DRY_RUN=1
    prints what it would copy, restamp and leave alone, per package, without
    changing anything."""
    seed = os.path.realpath(seed)
    worktree = os.path.realpath(worktree)
    target = os.path.realpath(target_dir or os.path.join(worktree, "target"))
    debug = os.path.join(target, "debug")
    seed_debug = os.path.join(seed, "target", "debug")
    if not os.path.isdir(os.path.join(debug, ".fingerprint")):
        return 0
    # Seed directories are never reused, so name + the branch's merge-base
    # with master identify the work. The branch's own commits leave that base
    # alone: every edit loop commits before agent-check, and keying on HEAD
    # re-hashed every source per edit (3 s idle, 7-10 s under load, a quarter
    # of a 24 s loop on 2026-09-25) to find nothing new, since what the branch
    # itself changed never matches the seed. A rebase moves the base and a
    # landing publishes a new seed, and both refresh again. Skipping a refresh
    # only forgoes reuse; Cargo still decides freshness.
    base = subprocess.run(["git", "-C", worktree, "merge-base", "HEAD", "refs/heads/master"],
                          capture_output=True, text=True).stdout.strip()
    if not base:
        base = subprocess.run(["git", "-C", worktree, "rev-parse", "HEAD"], check=True,
                              capture_output=True, text=True).stdout.strip()
    key = f"{os.path.basename(seed)} {base}\n"
    stamp_path = os.path.join(target, REFRESH_STAMP)
    try:
        with open(stamp_path) as f:
            if f.read() == key:
                return 0
    except OSError:
        pass
    manifest = load_manifest(seed)
    lock = open(os.path.join(debug, ".cargo-lock"), "a")
    try:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except OSError:
        print("build-seed: target busy; not refreshing", file=sys.stderr)
        return 0
    started = time.time()

    # Every file a refreshed unit reads must be older than this instant; the
    # refreshed units are then stamped with it. An edit made from now on is
    # newer than the stamp, and one made before is caught by the hash.
    stamp_ns = time.time_ns()
    files = tracked(worktree)
    untracked = [os.fsdecode(e) for e in subprocess.run(
        ["git", "-C", worktree, "ls-files", "-z", "--others", "--exclude-standard"],
        check=True, capture_output=True).stdout.split(b"\0") if e]
    known, known_dirs = manifest["files"], manifest["dirs"]
    our_dirs = directories(files)
    hashed = {}

    def hash_files(rels):
        """Blob ids of the WORKING files (not the index, which neither sees
        an unstaged edit under assume-unchanged nor a save racing git)."""
        todo = sorted(r for r in rels if r not in hashed)
        present = [r for r in todo if os.path.isfile(os.path.join(worktree, r))]
        for r in todo:
            hashed[r] = None
        if present:
            out = subprocess.run(["git", "-C", worktree, "hash-object", "--stdin-paths"],
                                 input="\n".join(present) + "\n", check=True,
                                 capture_output=True, text=True).stdout.split()
            hashed.update(zip(present, out))

    def same(rel):
        entry = known.get(rel)
        if entry is None or hashed.get(rel) != entry[0]:
            return False
        try:
            return os.lstat(os.path.join(worktree, rel)).st_mtime_ns < stamp_ns
        except FileNotFoundError:
            return False

    hash_files(r for r in GLOBAL_INPUTS if r in files or r in known)
    if not all(same(rel) for rel in GLOBAL_INPUTS if rel in files or rel in known):
        print(f"build-seed: Cargo manifests or config differ from seed "
              f"{manifest['head'][:9]}; not refreshing", file=sys.stderr)
        return write_stamp(stamp_path, key)

    packages = workspace_package_roots(worktree)
    gate = manifest["checkout"].rstrip("/") + "/"
    gate_target = os.path.realpath(manifest["target"]).rstrip("/") + "/"
    allowed = allowed_roots(worktree)
    path_bound = {os.path.basename(u) for u in manifest.get("path_bound_units", [])}
    seed_units = fingerprint_units(seed_debug)
    ours = fingerprint_units(debug)

    def package_of(unit):
        match = UNIT_DIR.match(unit)
        return match.group(1) if match and match.group(1) in packages else None

    by_hash = {}
    for unit, info in seed_units.items():
        for h in info["hashes"]:
            by_hash.setdefault(h, unit)

    def sources(unit, info):
        """(checkout files, directories) the unit's freshness reads, or None
        unless it was fresh against them in the gate."""
        root = os.path.relpath(packages[package_of(unit)], worktree)
        pkg_gate = os.path.normpath(os.path.join(gate, root))
        names, dirs, reference = set(), set(), None
        for local in info["json"][0].get("local", []):
            if "CheckDepInfo" in local:
                dep = os.path.join(seed, "target", local["CheckDepInfo"]["dep_info"])
                try:
                    with open(dep, "rb") as f:
                        decoded = decode_dep_info(f.read())
                    ref = os.stat(dep).st_mtime_ns
                except OSError:
                    return None
                if decoded is None:
                    return None
                for kind, encoded in decoded[0]:
                    p = os.fsdecode(encoded)
                    if not os.path.isabs(p):
                        p = os.path.join(pkg_gate if kind == 0 else seed_debug, p)
                    p = os.path.normpath(p)
                    if p.startswith(gate_target) or p.startswith(seed + "/"):
                        continue  # generated: owned by a dependency unit
                    elif p.startswith(gate):
                        names.add(os.path.relpath(p, gate))
                    elif foreign_path(os.fsencode(p), allowed):
                        return None
            elif "RerunIfChanged" in local:
                rerun = local["RerunIfChanged"]
                try:
                    ref = os.stat(os.path.join(seed, "target", rerun["output"])).st_mtime_ns
                except OSError:
                    return None
                for path in rerun["paths"]:
                    prefix = os.path.normpath(os.path.join(root, path))
                    under = [r for r in files if r == prefix or r.startswith(prefix + "/")]
                    if not under or any(u == prefix or u.startswith(prefix + "/")
                                        for u in untracked):
                        return None
                    names.update(under)
                    subdirs = {d for d in set(known_dirs) | set(our_dirs)
                               if d == prefix or d.startswith(prefix + "/")}
                    if any(our_dirs.get(d) != known_dirs.get(d, [None])[0] for d in subdirs):
                        return None  # a file added or removed under it
                    dirs.update(subdirs)
            elif "RerunIfEnvChanged" in local:
                continue  # cargo compares the variable itself
            else:
                return None
            reference = ref if reference is None else min(reference, ref)
        if reference is None or any(rel not in known or known[rel][1] > reference
                                    for rel in names):
            return None
        return names, dirs

    # Candidates: workspace units that were fresh in the gate, and
    # third-party units this target lacks entirely (registry sources never
    # change under a version; a feature or lockfile change names a new unit).
    candidates = {}
    for unit, info in seed_units.items():
        if unit in path_bound or len(info["json"]) != 1 \
                or not isinstance(info["json"][0], dict) or len(info["hashes"]) != 1:
            continue
        if package_of(unit) is None:
            reads = (set(), set()) if unit not in ours and UNIT_DIR.match(unit) else None
        else:
            reads = sources(unit, info)
        deps = dep_hashes(info)
        if reads is not None and deps is not None:
            candidates[unit] = (reads, deps)
    hash_files(set().union(*(reads[0] for reads, _ in candidates.values())))

    def reads_match(reads):
        names, dirs = reads
        if not all(same(rel) for rel in names):
            return False
        for d in dirs:
            try:
                if os.stat(os.path.join(worktree, d)).st_mtime_ns >= stamp_ns:
                    return False
            except FileNotFoundError:
                return False
        return True

    good = {u for u, (reads, _) in candidates.items() if reads_match(reads)}

    def dep_ready(h):
        unit = by_hash.get(h)
        if unit is None:
            return False
        if unit in good:
            return True
        mine = ours.get(unit)
        return package_of(unit) is None and mine is not None and h in mine["hashes"]

    changed = True
    while changed:
        changed = False
        for unit in list(good):
            if not all(dep_ready(h) for h in candidates[unit][1]):
                good.discard(unit)
                changed = True

    seed_index, our_index = index_deps(seed_debug), index_deps(debug)

    def identical(unit):
        """This target already holds the seed's copy (same clone lineage)."""
        mine = ours.get(unit)
        if mine is None or mine["hashes"] != seed_units[unit]["hashes"] \
                or sorted(mine["files"]) != sorted(seed_units[unit]["files"]):
            return False
        seed_out = sorted(unit_outputs(seed_debug, unit, seed_index))
        if seed_out != sorted(unit_outputs(debug, unit, our_index)):
            return False
        for rel in seed_out:
            try:
                a, b = os.stat(os.path.join(seed_debug, rel)), os.stat(os.path.join(debug, rel))
            except OSError:
                return False
            if (a.st_size, a.st_mtime_ns) != (b.st_size, b.st_mtime_ns):
                return False
        return True

    def fresh_here(unit):
        """Cargo would already find this target's unit fresh against the
        worktree's sources (so leaving it alone rebuilds nothing)."""
        (names, dirs), _ = candidates[unit]
        refs = []
        for local in ours[unit]["json"][0].get("local", []) if isinstance(
                ours[unit]["json"][0], dict) else [None]:
            if not isinstance(local, dict):
                return False
            if "CheckDepInfo" in local:
                refs.append(os.path.join(target, local["CheckDepInfo"]["dep_info"]))
            elif "RerunIfChanged" in local:
                refs.append(os.path.join(target, local["RerunIfChanged"]["output"]))
        try:
            reference = min(os.stat(r).st_mtime_ns for r in refs) if refs else None
            newest = max((os.stat(os.path.join(worktree, r)).st_mtime_ns
                          for r in names | dirs), default=0)
        except (OSError, ValueError):
            return False
        return reference is not None and newest <= reference

    # A unit is left alone when it is already the seed's and fresh here;
    # otherwise it is copied (unless identical) and stamped. Stamping moves
    # its outputs forward, so every good unit above it is stamped too.
    copy = {u for u in good if not identical(u)}
    stamp = set(copy) | {u for u in good - copy if package_of(u) and not fresh_here(u)}
    changed = True
    while changed:
        changed = False
        stamped_hashes = set().union(*(seed_units[u]["hashes"] for u in stamp)) \
            if stamp else set()
        for unit in good - stamp:
            if candidates[unit][1] & stamped_hashes:
                stamp.add(unit)
                changed = True
    if os.environ.get("WITCHY_SEED_REFRESH_DRY_RUN"):
        for label, units in (("copy", copy), ("stamp only", stamp - copy),
                             ("left alone", good - stamp)):
            by_package = {}
            for unit in units:
                name = package_of(unit) or "third-party"
                by_package[name] = by_package.get(name, 0) + 1
            print(f"{label}: {dict(sorted(by_package.items(), key=str))}")
        return 0

    # Swap: the old fingerprint goes first, so an interrupted swap leaves no
    # unit cargo would trust; the new one is renamed in only when complete.
    old = os.fsencode(gate)
    new = os.fsencode(worktree.rstrip("/") + "/")
    scratch = os.path.join(debug, f".seed-refresh-{os.getpid()}")
    copied = 0
    for unit in copy:
        shutil.rmtree(os.path.join(debug, ".fingerprint", unit), ignore_errors=True)
        for rel in unit_outputs(debug, unit, our_index):
            try:
                os.unlink(os.path.join(debug, rel))
            except FileNotFoundError:
                pass
        shutil.rmtree(os.path.join(debug, "build", unit), ignore_errors=True)
        for rel in unit_outputs(seed_debug, unit, seed_index):
            clone(os.path.join(seed_debug, rel), os.path.join(debug, rel))
        tmp = os.path.join(scratch, unit)
        for name in seed_units[unit]["files"]:
            clone(os.path.join(seed_debug, ".fingerprint", unit, name), os.path.join(tmp, name))
        if rebase_unit(tmp, old, new, allowed) != "dropped":
            os.rename(tmp, os.path.join(debug, ".fingerprint", unit))
            copied += 1
    shutil.rmtree(scratch, ignore_errors=True)
    # One instant for every stamped unit: cargo's dependency check is "a
    # dependency's output is NEWER than ours", so equal times are fresh, and
    # a source is stale only when newer than the dep-info.
    now_index = index_deps(debug)
    for unit in stamp:
        paths = [os.path.join(debug, rel) for rel in unit_outputs(debug, unit, now_index)
                 if not os.path.islink(os.path.join(debug, rel))]
        fingerprint = os.path.join(debug, ".fingerprint", unit)
        paths += [os.path.join(fingerprint, n) for n in os.listdir(fingerprint)] \
            if os.path.isdir(fingerprint) else []
        for path in paths:
            try:
                os.utime(path, ns=(stamp_ns, stamp_ns))
            except FileNotFoundError:
                pass
    write_stamp(stamp_path, key)
    print(f"build-seed: refreshed {copied} unit(s) from seed {manifest['head'][:9]} "
          f"({len(stamp) - len(copy)} restamped, {len(good - stamp)} already current) in "
          f"{time.time() - started:.1f}s", file=sys.stderr)
    return 0

if __name__ == "__main__":
    if len(sys.argv) == 5 and sys.argv[1] == "publish":
        sys.exit(publish(*sys.argv[2:]))
    if len(sys.argv) == 3 and sys.argv[1] == "finish":
        sys.exit(finish(sys.argv[2]))
    if len(sys.argv) in (4, 5) and sys.argv[1] == "apply":
        sys.exit(apply(*sys.argv[2:]))
    if len(sys.argv) in (4, 5) and sys.argv[1] == "refresh":
        sys.exit(refresh(*sys.argv[2:]))
    if len(sys.argv) == 4 and sys.argv[1] == "snapshot-mtimes":
        sys.exit(snapshot_mtimes(*sys.argv[2:]))
    if len(sys.argv) == 4 and sys.argv[1] == "restore-mtimes":
        sys.exit(restore_mtimes(*sys.argv[2:]))
    print(__doc__, file=sys.stderr)
    sys.exit(2)
