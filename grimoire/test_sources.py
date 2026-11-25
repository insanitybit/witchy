"""Freeze bounded source members using held directory descriptors before checking private edges."""

import hashlib
import os
from pathlib import Path
import stat
import unicodedata

DIR = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC
FILE = os.O_RDONLY | os.O_NONBLOCK | os.O_NOFOLLOW | os.O_CLOEXEC
EXCLUDED = {".git", ".hg", ".svn", "target", "__pycache__", ".grimoire", ".coven"}


def regular(directory, name, limit):
    with os.fdopen(os.open(name, FILE, dir_fd=directory), "rb") as f:
        meta = os.fstat(f.fileno())
        if not stat.S_ISREG(meta.st_mode) or meta.st_nlink != 1 or meta.st_size > limit:
            raise ValueError(f"invalid or oversized source member {name}")
        value = f.read(limit + 1)
        again = os.fstat(f.fileno())
        if (
            len(value) > limit
            or meta.st_ctime_ns != again.st_ctime_ns
            or len(value) != meta.st_size
        ):
            raise ValueError(f"source member changed during admission: {name}")
        return value


def opened_directory(path):
    # Each component is resolved from its opened parent, including the package root.
    held = os.open("/", DIR)
    try:
        for component in Path(path).absolute().parts[1:]:
            if component == "..":
                raise ValueError("noncanonical package root")
            child = os.open(component, DIR, dir_fd=held)
            os.close(held)
            held = child
        return held
    except BaseException:
        os.close(held)
        raise


def inventory(root, *, foreign_roots=(), release_only=False):
    native_release = release_only and (root / "test.wy").is_file()
    members = {}
    total = visited = 0

    def walk(fd, prefix):
        nonlocal total, visited
        try:
            if len(prefix.parts) > 16:
                raise ValueError("package inventory depth exceeds 16")
            names = set()
            with os.scandir(fd) as entries:
                for entry in entries:
                    visited += 1
                    if visited > 16384:
                        raise ValueError("package inventory exceeds 16384 entries")
                    name = entry.name
                    folded = unicodedata.normalize("NFC", name).casefold()
                    if folded in names:
                        raise ValueError("ambiguous package member spelling")
                    names.add(folded)
                    rel = prefix / name
                    if native_release and (
                        rel == Path("test.wy") or rel == Path("tests")
                    ):
                        continue
                    if (
                        name in EXCLUDED
                        or rel == Path("tests/reject")
                        or (root / rel) in foreign_roots
                    ):
                        continue
                    if entry.is_dir(follow_symlinks=False):
                        sub = os.open(name, DIR, dir_fd=fd)
                        try:
                            os.stat("witchy.toml", dir_fd=sub, follow_symlinks=False)
                        except FileNotFoundError:
                            walk(sub, rel)
                        else:
                            os.close(sub)
                        continue
                    if rel.suffix not in {".wy", ".policy", ".packages"}:
                        continue
                    if (
                        len(members) >= 512
                        or "::" in name
                        or any(ord(c) < 32 for c in name)
                    ):
                        raise ValueError("invalid package member name or count")
                    value = regular(fd, name, min(1 << 20, (8 << 20) - total))
                    members[rel] = value
                    total += len(value)
        finally:
            os.close(fd)

    walk(opened_directory(root), Path())
    return members


def write_members(dest, members):
    for member, data in members.items():
        path = dest / member
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)


def local_packages(api, local):
    if local is None or not local.exists():
        return {}
    held = opened_directory(local.parent)
    try:
        listing = regular(held, local.name, 65536).decode()
    finally:
        os.close(held)
    direct = {}
    for line in listing.splitlines():
        text = line.partition("#")[0].strip()
        if not text:
            continue
        head, sep, tail = text.partition(":")
        words = head.split()
        if (
            not sep
            or len(words) not in (1, 2)
            or not api.NAME.fullmatch(words[0])
            or words[0] in direct
        ):
            raise ValueError("invalid or duplicate local test package")
        target, _, edges = tail.partition(";")
        source = (local.parent / target.strip()).absolute()
        if any(p.is_symlink() for p in [source, *source.parents]):
            raise ValueError("local package path contains a symbolic link")
        source = source.resolve(strict=True)
        declaring = next(
            (p for p in source.parents if (p / "witchy.toml").is_file()), None
        )
        if (
            declaring
            and (declaring / "test.wy").is_file()
            and (
                source.samefile(declaring / "test.wy")
                or source.is_relative_to(declaring / "tests")
            )
        ):
            raise ValueError("a local dependency entry cannot be a package test member")
        direct[words[0]] = (head.strip(), source, edges.strip() or "requires")
    return direct


def stage(api, manifest, work, doc, lock_bytes):
    root = manifest.parent
    registry, deps = api.read_dependencies(manifest, project=doc)
    test_list = root / "test.packages"
    release = doc.get("run", {}).get("entry", doc.get("package", {}).get("source"))
    release_list = None
    if release is not None:
        candidate = Path(release)
        if candidate.is_absolute() or ".." in candidate.parts:
            raise ValueError("release entry outside package")
        release_list = (root / candidate).with_suffix(".packages")
    copies = []
    foreign = set()
    if deps:
        if test_list.exists() or (release_list and release_list.exists()):
            raise ValueError("local package list conflicts with registry resolution")
        graph = api.read_lock(manifest, deps, registry, work, lock_bytes=lock_bytes)
        production = tests = api.package_listing(graph)
        direct = set(deps)
        production_direct = direct
    else:
        release_edges = local_packages(api, release_list)
        test_edges = local_packages(api, test_list)
        for name in test_edges.keys() & release_edges.keys():
            if test_edges[name] != release_edges[name]:
                raise ValueError("conflicting local dependency identity: " + name)
        selected = release_edges | test_edges
        if len(selected) > 64:
            raise ValueError("too many local dependency roots")
        lines = {}
        for name, (head, source, edges) in selected.items():
            owner = source.parent
            if (
                owner == root
                or root.is_relative_to(owner)
                or any(
                    owner.is_relative_to(p) or p.is_relative_to(owner) for p in foreign
                )
            ):
                raise ValueError("overlapping local package owners")
            foreign.add(owner)
            copy = work / "dependencies" / name
            members = inventory(owner, release_only=True)
            if Path(source.name) not in members:
                raise ValueError("invalid local package entry")
            write_members(copy, members)
            copies.append((owner, members))
            lines[name] = f"{head}: {copy / source.name} ; {edges}"
        production = [lines[name] for name in release_edges]
        tests = list(lines.values())
        direct = set(selected)
        production_direct = set(release_edges)
    frozen = inventory(root, foreign_roots=foreign)
    if tests and Path("test.policy") not in frozen:
        raise ValueError("dependencies require test.policy with reviewed file ceilings")
    if (
        "run" in doc
        and release_list
        and (production or release_list.exists())
        and release_list.with_suffix(".policy").relative_to(root) not in frozen
    ):
        raise ValueError("production dependencies require the executable's policy")
    dest = work / "owner"
    write_members(dest, {p: b for p, b in frozen.items() if p.suffix != ".packages"})
    if tests:
        (dest / "test.packages").write_text("\n".join(tests) + "\n")
    if production and release_list:
        (dest / release_list.relative_to(root)).write_text("\n".join(production) + "\n")
    if release_list:
        (dest / release_list.relative_to(root).with_suffix(".direct")).write_text(
            "\n".join(sorted(production_direct))
        )
    (dest / "test.direct").write_text("\n".join(sorted(direct)))
    admitted = [(str(p), frozen[p]) for p in sorted(frozen) if p.suffix == ".packages"]
    owners = [root, *sorted(foreign)]

    def identity(path):
        st = path.stat()
        return str(path), st.st_dev, st.st_ino

    identities = [identity(p) for p in owners]
    origin = hashlib.sha256(repr((identities, admitted)).encode()).hexdigest()

    def current():
        if identities != [identity(p) for p in owners]:
            raise ValueError("resolved package owner changed during checking")
        if inventory(root, foreign_roots=foreign) != frozen or any(
            inventory(owner, release_only=True) != members for owner, members in copies
        ):
            raise ValueError("package sources changed during test build")

    return dest / "test.wy", current, origin
