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


def rebase_dep_info(target, old_root, new_root):
    """Point absolute dep-info paths under old_root at new_root, and drop the
    fingerprint of any unit that would still name a file outside this
    worktree, the cargo home or the toolchain (e.g. the gate checkout spelled
    another way). Each file is replaced atomically. Returns (rewritten,
    dropped) unit counts."""
    old = os.fsencode(old_root.rstrip("/")) + b"/"
    new = os.fsencode(new_root.rstrip("/")) + b"/"
    cargo_home = os.environ.get("CARGO_HOME") or os.path.expanduser("~/.cargo")
    sysroot = subprocess.run(["rustc", "--print", "sysroot"], capture_output=True,
                             text=True).stdout.strip()
    allowed = [new] + [os.fsencode(os.path.realpath(p).rstrip("/")) + b"/"
                       for p in (cargo_home, sysroot) if p]
    allowed += [b"/opt/homebrew/", b"/usr/", b"/Library/", b"/System/", b"/Applications/"]
    rewritten = dropped = 0
    for top, dirs, _ in os.walk(target):
        dirs[:] = [d for d in dirs if d != "incremental"]
        if os.path.basename(top) != ".fingerprint":
            continue
        units, dirs[:] = list(dirs), []
        for unit in units:
            unit_dir = os.path.join(top, unit)
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
                        dropped += 1
                        break
                    continue
                files = [(kind, new + p[len(old):] if p.startswith(old) else p)
                         for kind, p in decoded[0]]
                if any(foreign_path(p, allowed) for _, p in files):
                    shutil.rmtree(unit_dir, ignore_errors=True)  # cargo rebuilds it
                    dropped += 1
                    break
                if files == decoded[0]:
                    continue
                stat = os.stat(path)
                tmp = f"{path}.seed-{os.getpid()}"
                with open(tmp, "wb") as f:
                    f.write(encode_dep_info(files, decoded[1]))
                os.utime(tmp, ns=(stat.st_atime_ns, stat.st_mtime_ns))
                os.replace(tmp, path)
                rewritten += 1
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


if __name__ == "__main__":
    if len(sys.argv) == 5 and sys.argv[1] == "publish":
        sys.exit(publish(*sys.argv[2:]))
    if len(sys.argv) == 3 and sys.argv[1] == "finish":
        sys.exit(finish(sys.argv[2]))
    if len(sys.argv) in (4, 5) and sys.argv[1] == "apply":
        sys.exit(apply(*sys.argv[2:]))
    print(__doc__, file=sys.stderr)
    sys.exit(2)
