#!/usr/bin/env python3
"""Install and authenticate Witchy's reviewed Binaryen version_131 bundle."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import shutil
import stat
import subprocess
import sys
import tarfile
import tempfile
import tomllib
import urllib.request


REPO = pathlib.Path(__file__).resolve().parent.parent
LOCK = REPO / "tools" / "binaryen-version_131.lock"
TARGET_TO_PLATFORM = {
    "aarch64-unknown-linux-gnu": "aarch64-linux",
    "x86_64-unknown-linux-gnu": "x86_64-linux",
    "aarch64-apple-darwin": "arm64-macos",
    "x86_64-apple-darwin": "x86_64-macos",
}
MAX_ARCHIVE_BYTES = 512 * 1024 * 1024


def fail(message: str) -> "NoReturn":
    raise SystemExit(f"pinned-binaryen: {message}")


def sha256_file(path: pathlib.Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def load_asset(target: str) -> tuple[dict[str, object], dict[str, object]]:
    try:
        lock = tomllib.loads(LOCK.read_text(encoding="utf-8"))
    except (OSError, tomllib.TOMLDecodeError) as error:
        fail(f"cannot read reviewed lock: {error}")
    if lock.get("schema") != 1 or lock.get("version") != 131:
        fail("reviewed lock has an unsupported schema or version")
    platform = TARGET_TO_PLATFORM.get(target)
    if platform is None:
        fail(f"unsupported target {target!r}")
    matches = [asset for asset in lock.get("asset", []) if asset.get("platform") == platform]
    if len(matches) != 1:
        fail(f"reviewed lock does not contain exactly one asset for {platform}")
    asset = matches[0]
    required = ("url", "sha256", "executable_sha256")
    if any(not isinstance(asset.get(key), str) for key in required):
        fail(f"reviewed lock asset for {platform} is incomplete")
    url = str(asset["url"])
    expected_prefix = "https://github.com/WebAssembly/binaryen/releases/download/version_131/"
    if not url.startswith(expected_prefix):
        fail("reviewed asset URL is outside the pinned Binaryen release")
    return lock, asset


def regular_file(path: pathlib.Path, label: str) -> None:
    try:
        metadata = path.lstat()
    except OSError as error:
        fail(f"cannot inspect {label}: {error}")
    if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISREG(metadata.st_mode):
        fail(f"{label} is not a regular non-symlink file: {path}")


def verify_bundle(target: str, root: pathlib.Path) -> dict[str, str]:
    lock, asset = load_asset(target)
    try:
        root = root.resolve(strict=True)
    except OSError as error:
        fail(f"bundle root is unavailable: {error}")
    executable = root / "bin" / "wasm-opt"
    regular_file(executable, "wasm-opt")
    executable_digest = sha256_file(executable)
    if executable_digest != asset["executable_sha256"]:
        fail(
            "wasm-opt digest mismatch: "
            f"expected {asset['executable_sha256']}, got {executable_digest}"
        )
    library_digest = ""
    library_name = asset.get("runtime_library")
    if library_name is not None:
        if library_name != "lib/libbinaryen.dylib":
            fail("reviewed runtime library has an unexpected path")
        library = root / str(library_name)
        regular_file(library, "Binaryen runtime library")
        library_digest = sha256_file(library)
        if library_digest != asset.get("runtime_library_sha256"):
            fail(
                "runtime-library digest mismatch: "
                f"expected {asset.get('runtime_library_sha256')}, got {library_digest}"
            )
    elif (root / "lib").exists():
        fail("Linux reviewed bundle unexpectedly contains a runtime library directory")

    try:
        version = subprocess.run(
            [os.fspath(executable), "--version"],
            cwd=root,
            env={},
            check=False,
            capture_output=True,
            timeout=30,
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        fail(f"cannot execute authenticated wasm-opt: {error}")
    expected_version = str(lock.get("version_stdout", "")).encode()
    if version.returncode != 0 or version.stdout != expected_version or version.stderr:
        fail(
            "wasm-opt version evidence mismatch: "
            f"status={version.returncode} stdout={version.stdout!r} stderr={version.stderr!r}"
        )
    return {
        "root": os.fspath(root),
        "wasm_opt_path": os.fspath(executable),
        "wasm_opt_sha256": executable_digest,
        "binaryen_lib_sha256": library_digest,
        "platform": str(asset["platform"]),
        "archive_sha256": str(asset["sha256"]),
        "release": str(lock["release_tag"]),
        "source_commit": str(lock["source_commit"]),
    }


def download(url: str, destination: pathlib.Path) -> None:
    request = urllib.request.Request(url, headers={"User-Agent": "witchy-release/1"})
    try:
        with urllib.request.urlopen(request, timeout=60) as response, destination.open("xb") as out:
            total = 0
            while chunk := response.read(1024 * 1024):
                total += len(chunk)
                if total > MAX_ARCHIVE_BYTES:
                    fail("reviewed archive exceeds the 512 MiB safety bound")
                out.write(chunk)
    except OSError as error:
        fail(f"cannot download reviewed archive: {error}")


def install(target: str, output: pathlib.Path, archive: pathlib.Path | None) -> dict[str, str]:
    lock, asset = load_asset(target)
    if output.exists() or output.is_symlink():
        fail(f"output path already exists: {output}")
    output.parent.mkdir(parents=True, exist_ok=True)
    scratch = pathlib.Path(tempfile.mkdtemp(prefix=".binaryen-v131-", dir=output.parent))
    try:
        if archive is None:
            archive = scratch / "binaryen-version_131.tar.gz"
            download(str(asset["url"]), archive)
        else:
            regular_file(archive, "reviewed archive")
        archive_digest = sha256_file(archive)
        if archive_digest != asset["sha256"]:
            fail(
                "archive digest mismatch: "
                f"expected {asset['sha256']}, got {archive_digest}"
            )

        distribution = f"binaryen-{lock['release_tag']}"
        expected = [f"{distribution}/bin/wasm-opt"]
        if runtime_library := asset.get("runtime_library"):
            expected.append(f"{distribution}/{runtime_library}")
        extracted = scratch / "bundle"
        extracted.mkdir(mode=0o700)
        try:
            with tarfile.open(archive, mode="r:gz") as tar:
                members = tar.getmembers()
                for name in expected:
                    matches = [member for member in members if member.name == name]
                    if len(matches) != 1:
                        fail(
                            "reviewed archive does not contain exactly one member "
                            f"{name!r}"
                        )
                    member = matches[0]
                    if not member.isfile() or member.issym() or member.islnk():
                        fail(f"reviewed archive member is not a regular file: {name!r}")
                    source = tar.extractfile(member)
                    if source is None:
                        fail(f"cannot read reviewed archive member {name!r}")
                    relative = pathlib.PurePosixPath(name).relative_to(distribution)
                    if any(part in ("", ".", "..") for part in relative.parts):
                        fail(f"unsafe reviewed archive member {name!r}")
                    destination = extracted.joinpath(*relative.parts)
                    destination.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
                    with destination.open("xb") as out:
                        shutil.copyfileobj(source, out, length=1024 * 1024)
                    destination.chmod(0o500 if relative.as_posix() == "bin/wasm-opt" else 0o400)
        except (OSError, tarfile.TarError) as error:
            fail(f"cannot extract reviewed archive: {error}")

        evidence = verify_bundle(target, extracted)
        os.replace(extracted, output)
        evidence["root"] = os.fspath(output.resolve(strict=True))
        evidence["wasm_opt_path"] = os.fspath(
            (output / "bin" / "wasm-opt").resolve(strict=True)
        )
        return evidence
    finally:
        shutil.rmtree(scratch, ignore_errors=True)


def append_lines(path: pathlib.Path, values: dict[str, str]) -> None:
    if path.exists():
        regular_file(path, "GitHub environment/output file")
    with path.open("a", encoding="utf-8") as output:
        for key, value in values.items():
            if "\n" in value or "\r" in value:
                fail(f"refusing multiline GitHub value for {key}")
            output.write(f"{key}={value}\n")


def main() -> None:
    parser = argparse.ArgumentParser()
    subparsers = parser.add_subparsers(dest="command", required=True)
    install_parser = subparsers.add_parser("install")
    install_parser.add_argument("--target", required=True)
    install_parser.add_argument("--output-dir", required=True, type=pathlib.Path)
    install_parser.add_argument("--archive", type=pathlib.Path)
    install_parser.add_argument("--github-output", type=pathlib.Path)
    install_parser.add_argument("--github-env", type=pathlib.Path)
    verify_parser = subparsers.add_parser("verify")
    verify_parser.add_argument("--target", required=True)
    verify_parser.add_argument("--bundle-root", required=True, type=pathlib.Path)
    show_parser = subparsers.add_parser("show")
    show_parser.add_argument("--target", required=True)
    args = parser.parse_args()

    if args.command == "install":
        evidence = install(args.target, args.output_dir, args.archive)
        if args.github_output:
            append_lines(args.github_output, evidence)
        if args.github_env:
            environment = {
                "WITCHY_PINNED_WASM_OPT_PATH": evidence["wasm_opt_path"],
                "WITCHY_PINNED_WASM_OPT_SHA256": evidence["wasm_opt_sha256"],
            }
            if evidence["binaryen_lib_sha256"]:
                environment["WITCHY_PINNED_BINARYEN_LIB_SHA256"] = evidence[
                    "binaryen_lib_sha256"
                ]
            append_lines(args.github_env, environment)
    elif args.command == "verify":
        evidence = verify_bundle(args.target, args.bundle_root)
    else:
        lock, asset = load_asset(args.target)
        evidence = {
            "platform": str(asset["platform"]),
            "url": str(asset["url"]),
            "archive_sha256": str(asset["sha256"]),
            "wasm_opt_sha256": str(asset["executable_sha256"]),
            "binaryen_lib_sha256": str(asset.get("runtime_library_sha256", "")),
            "release": str(lock["release_tag"]),
            "source_commit": str(lock["source_commit"]),
        }
    print(json.dumps(evidence, sort_keys=True))


if __name__ == "__main__":
    main()
