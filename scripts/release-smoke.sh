#!/usr/bin/env bash
# Verify an installed release archive without using compiler inputs from the checkout.
# shellcheck disable=SC2016 # Witchy fixture interpolation must remain literal.
set -euo pipefail

version=""
archive=""
checksums=""
commit=""
target=""

usage() {
    echo "usage: release-smoke.sh --version <X.Y.Z> --archive <tar.gz> --checksums <SHA256SUMS> --commit <40-hex-sha> --target <triple>" >&2
}
while [ "$#" -gt 0 ]; do
    case "$1" in
        --version | --archive | --checksums | --commit | --target)
            [ "$#" -ge 2 ] || { usage; exit 2; }
            case "$1" in
                --version) version="$2" ;;
                --archive) archive="$2" ;;
                --checksums) checksums="$2" ;;
                --commit) commit="$2" ;;
                --target) target="$2" ;;
            esac
            shift 2
            ;;
        -h | --help) usage; exit 0 ;;
        *) echo "release-smoke: unknown argument '$1'" >&2; usage; exit 2 ;;
    esac
done
[ -n "$version" ] && [ -n "$archive" ] && [ -n "$checksums" ] && [ -n "$commit" ] && [ -n "$target" ] || {
    usage
    exit 2
}
if [[ ! "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z][0-9A-Za-z.]*)?$ ]]; then
    echo "release-smoke: --version must look like X.Y.Z or X.Y.Z-pre" >&2
    exit 1
fi
if [[ ! "$commit" =~ ^[0-9a-f]{40}$ ]]; then
    echo "release-smoke: --commit must be exactly 40 lowercase hexadecimal characters" >&2
    exit 1
fi
case "$target" in
    x86_64-unknown-linux-gnu | aarch64-apple-darwin | x86_64-apple-darwin) ;;
    *) echo "release-smoke: unsupported target '$target'" >&2; exit 1 ;;
esac
expected_archive="witchy-${version}-${target}.tar.gz"
[ "$(basename "$archive")" = "$expected_archive" ] || {
    echo "release-smoke: archive name does not match target: $(basename "$archive")" >&2
    exit 1
}
[ -f "$archive" ] && [ ! -L "$archive" ] || {
    echo "release-smoke: archive is missing or is a symlink: $archive" >&2
    exit 1
}
[ -f "$checksums" ] && [ ! -L "$checksums" ] || {
    echo "release-smoke: checksum manifest is missing or is a symlink: $checksums" >&2
    exit 1
}

PYTHON="$(command -v python3)"
CP="$(command -v cp)"
RM="$(command -v rm)"
MKDIR="$(command -v mkdir)"
MKTEMP="$(command -v mktemp)"

scratch="$($MKTEMP -d "${TMPDIR:-/tmp}/witchy-release-smoke.XXXXXX")"
cleanup() { "$RM" -rf "$scratch"; }
trap cleanup EXIT HUP INT TERM

install="$scratch/install"
"$MKDIR" -p "$install"
"$PYTHON" - "$archive" "$checksums" "$install" "$target" "$version" <<'PY'
import hashlib
import os
import pathlib
import posixpath
import re
import sys
import tarfile

archive = pathlib.Path(sys.argv[1])
checksums = pathlib.Path(sys.argv[2])
install = pathlib.Path(sys.argv[3])
target = sys.argv[4]
version = sys.argv[5]
root = f"witchy-{version}-{target}"
expected = [
    root,
    f"{root}/bin",
    f"{root}/bin/witchy",
    f"{root}/README.md",
    f"{root}/CHANGELOG.md",
    f"{root}/LICENSE-MIT",
    f"{root}/LICENSE-APACHE",
]

manifest = {}
line_re = re.compile(r"([0-9a-f]{64})  ([^/\n]+)")
for raw in checksums.read_text(encoding="ascii").splitlines():
    match = line_re.fullmatch(raw)
    if not match:
        raise SystemExit(f"release-smoke: malformed SHA256SUMS line: {raw!r}")
    digest, name = match.groups()
    if name in manifest:
        raise SystemExit(f"release-smoke: duplicate SHA256SUMS entry: {name}")
    manifest[name] = digest
expected_digest = manifest.get(archive.name)
if expected_digest is None:
    raise SystemExit(f"release-smoke: SHA256SUMS has no entry for {archive.name}")
actual_digest = hashlib.sha256(archive.read_bytes()).hexdigest()
if actual_digest != expected_digest:
    raise SystemExit(
        f"release-smoke: archive digest mismatch: expected {expected_digest}, got {actual_digest}"
    )

def safe_name(name: str) -> bool:
    return (
        name
        and not name.startswith("/")
        and "\\" not in name
        and posixpath.normpath(name) == name
        and all(part not in ("", ".", "..") for part in name.split("/"))
    )

with tarfile.open(archive, mode="r:gz") as tar:
    members = tar.getmembers()
    if [member.name for member in members] != expected:
        raise SystemExit("release-smoke: archive contains missing, reordered, or unexpected members")
    for member in members:
        if not safe_name(member.name) or member.issym() or member.islnk():
            raise SystemExit(f"release-smoke: unsafe archive member: {member.name!r}")
        destination = install.joinpath(*member.name.split("/"))
        if member.isdir():
            if member.mode != 0o755:
                raise SystemExit(f"release-smoke: unsafe directory mode for {member.name}")
            destination.mkdir(mode=0o755, parents=True, exist_ok=False)
        elif member.isfile():
            expected_mode = 0o755 if member.name.endswith("/bin/witchy") else 0o644
            if member.mode != expected_mode:
                raise SystemExit(f"release-smoke: unsafe file mode for {member.name}")
            destination.parent.mkdir(mode=0o755, parents=True, exist_ok=True)
            source = tar.extractfile(member)
            if source is None:
                raise SystemExit(f"release-smoke: cannot read {member.name}")
            with destination.open("xb") as handle:
                for chunk in iter(lambda: source.read(1024 * 1024), b""):
                    handle.write(chunk)
            os.chmod(destination, expected_mode)
        else:
            raise SystemExit(f"release-smoke: unsupported archive member type: {member.name}")
PY

root="$install/witchy-${version}-${target}"
work="$scratch/work"
home="$scratch/home"
tmp="$scratch/tmp"
cache="$scratch/cache"
"$MKDIR" -p "$work" "$home" "$tmp" "$cache/stdlib"
export HOME="$home"
export TMPDIR="$tmp"
export XDG_CACHE_HOME="$cache"
export WITCHY_TEST_STDLIB_CACHE_DIR="$cache/stdlib"
export PATH="$root/bin"

[ "$(command -v witchy)" = "$root/bin/witchy" ] || {
    echo "release-smoke: extracted witchy is not the selected PATH binary" >&2
    exit 1
}
expected_version="witchy ${version} (commit ${commit})"
[ "$(witchy --version)" = "$expected_version" ] || {
    echo "release-smoke: extracted binary provenance mismatch" >&2
    exit 1
}

cd "$work"
printf '%s\n' \
    'fn main(console: Console):' \
    '    console.print("release-smoke")' > hello.witchy
witchy check hello.witchy >/dev/null
witchy fmt --check hello.witchy
[ "$(witchy hello.witchy)" = "release-smoke" ] || {
    echo "release-smoke: direct source execution produced unexpected output" >&2
    exit 1
}

printf '%s\n' \
    'fn main(console:Console):' \
    ' console.print("format-smoke")' > needs-format.witchy
if witchy fmt --check needs-format.witchy >/dev/null 2>&1; then
    echo "release-smoke: fmt --check accepted deliberately non-canonical source" >&2
    exit 1
fi
witchy fmt needs-format.witchy
witchy fmt --check needs-format.witchy
printf '%s\n' 'fn main(' > invalid.witchy
if witchy check invalid.witchy >/dev/null 2>&1; then
    echo "release-smoke: check accepted invalid source" >&2
    exit 1
fi

"$MKDIR" -p project/src
printf '%s\n' \
    '[rune]' \
    'name = "release_smoke"' \
    'version = "0.1.0"' \
    '' \
    '[dependencies]' > project/witchy.toml
printf '%s\n' \
    'pub fn answer() -> Int:' \
    '    42' \
    '' \
    'fn main(console: Console):' \
    '    console.print("project:${answer()}")' > project/src/release_smoke.witchy
printf '%s\n' \
    'import release_smoke' \
    'import testing' \
    '' \
    'fn test_answer():' \
    '    testing.assert_int_eq(release_smoke.answer(), 42)' > project/src/release_smoke_test.witchy
witchy test project >/dev/null
witchy build project >/dev/null
project_output="$(witchy run project)"
project_line="${project_output%%$'\n'*}"
[ "$project_line" = "project:42" ] || {
    echo "release-smoke: project run produced unexpected output" >&2
    printf '%s\n' "$project_output" >&2
    exit 1
}
confinement_reports="${project_output#"$project_line"}"
confinement_reports="${confinement_reports#$'\n'}"
if [ -n "$confinement_reports" ]; then
    while IFS= read -r report; do
        case "$report" in
            "confinement: layer="*) ;;
            *)
                echo "release-smoke: project run produced unexpected trailing output" >&2
                printf '%s\n' "$project_output" >&2
                exit 1
                ;;
        esac
    done <<<"$confinement_reports"
fi

printf '%s\n' \
    'capability ReleaseFixture:' \
    '    console: Console' \
    '    clock: Clock' \
    '    args: List(String)' \
    '' \
    'fn test_extracted_fixture(root: ReleaseFixture):' \
    '    match root:' \
    '        ReleaseFixture(console, clock, args) ->' \
    '            console.print("${clock.now()}:${args.at(0)}")' > fixture.witchy
printf '%s\n' \
    '{"version":1,"console":{},"clock":{"start_ns":"42000000","step_ns":"1"},"argv":["artifact"],"expectations":{"calls":[{"family":"console","operation":"print","minimum":"1","maximum":"1"},{"family":"clock","operation":"now","minimum":"1","maximum":"1"},{"family":"argv","operation":"args","minimum":"1","maximum":"1"}]}}' > fixture.json
fixture_output="$(
    witchy test --fixtures fixture.json --backend both \
        --filter extracted_fixture --show-output fixture.witchy
)"
case "$fixture_output" in
    *"test fixture.test_extracted_fixture ... ok"*"42:artifact"*"1 passed; 0 failed"*) ;;
    *)
        echo "release-smoke: extracted fixture parity produced unexpected output" >&2
        printf '%s\n' "$fixture_output" >&2
        exit 1
        ;;
esac

witchy compile hello.witchy --out portable.wasm >/dev/null
[ -s portable.wasm ] || { echo "release-smoke: portable WASM was not written" >&2; exit 1; }
[ "$(witchy sandbox portable.wasm)" = "release-smoke" ] || {
    echo "release-smoke: portable WASM sandbox produced unexpected output" >&2
    exit 1
}

"$MKDIR" -p trusted/src invalid-binding/src incompatible-binding/src installed run-root
printf '%s\n' \
    '[rune]' \
    'name = "trusted_smoke"' \
    'version = "0.1.0"' \
    '' \
    '[targets.trusted-exe.dirs]' \
    'cwd = { from = "cwd" }' > trusted/witchy.toml
printf '%s\n' \
    'import list' \
    '' \
    'fn main(console: Console, cwd: Dir[Read], args: List(String)):' \
    '    console.print("main-entered")' \
    '    console.print("data:${cwd.read(list.at(args, 0))}")' \
    '    console.print("argv:${list.join(args, "|")}")' > trusted/src/trusted_smoke.witchy

"$CP" trusted/src/trusted_smoke.witchy invalid-binding/src/trusted_smoke.witchy
printf '%s\n' \
    '[rune]' \
    'name = "trusted_smoke"' \
    'version = "0.1.0"' > invalid-binding/witchy.toml
if witchy --release build --target trusted-exe invalid-binding >missing-binding.err 2>&1; then
    echo "release-smoke: trusted-exe build accepted a missing binding" >&2
    exit 1
fi
case "$(<missing-binding.err)" in
    *'missing `[targets.trusted-exe.dirs].cwd`'*) ;;
    *) echo "release-smoke: missing binding failed for the wrong reason" >&2; exit 1 ;;
esac

"$CP" trusted/src/trusted_smoke.witchy incompatible-binding/src/trusted_smoke.witchy
printf '%s\n' \
    '[rune]' \
    'name = "trusted_smoke"' \
    'version = "0.1.0"' \
    '' \
    '[targets.trusted-exe.files]' \
    'cwd = { from = "path", path = "inside.txt" }' > incompatible-binding/witchy.toml
if witchy --release build --target trusted-exe incompatible-binding >incompatible-binding.err 2>&1; then
    echo "release-smoke: trusted-exe build accepted an incompatible binding" >&2
    exit 1
fi
case "$(<incompatible-binding.err)" in
    *'type-incompatible'*) ;;
    *) echo "release-smoke: incompatible binding failed for the wrong reason" >&2; exit 1 ;;
esac

witchy --release build --target trusted-exe trusted >/dev/null
trusted_built="trusted/target/release/trusted_smoke"
[ -x "$trusted_built" ] || { echo "release-smoke: trusted executable was not built" >&2; exit 1; }
"$CP" "$trusted_built" installed/trusted-smoke
"$RM" -rf trusted invalid-binding incompatible-binding project
printf '%s' 'inside-data' > run-root/inside.txt
printf '%s' 'outside-data' > outside.txt

trusted="$work/installed/trusted-smoke"
trusted_output="$(cd run-root && PATH='' "$trusted" inside.txt --dir looks-like-grant --net example.invalid:1)"
expected_output="main-entered
data:inside-data
argv:inside.txt|--dir|looks-like-grant|--net|example.invalid:1"
[ "$trusted_output" = "$expected_output" ] || {
    echo "release-smoke: installed trusted executable output mismatch" >&2
    printf '%s\n' "$trusted_output" >&2
    exit 1
}

if (cd run-root && PATH='' "$trusted" ../outside.txt >escape.out 2>escape.err); then
    echo "release-smoke: trusted executable allowed '..' to escape its Dir" >&2
    exit 1
fi
case "$(<run-root/escape.err)" in
    *'`..` escapes the Dir capability'*) ;;
    *) echo "release-smoke: parent escape failed for the wrong reason" >&2; exit 1 ;;
esac
if (cd run-root && PATH='' "$trusted" /inside.txt >absolute.out 2>absolute.err); then
    echo "release-smoke: trusted executable allowed an absolute guest path" >&2
    exit 1
fi
case "$(<run-root/absolute.err)" in
    *'absolute paths are not allowed'*) ;;
    *) echo "release-smoke: absolute escape failed for the wrong reason" >&2; exit 1 ;;
esac

"$PYTHON" - "$trusted" "$work/installed/trusted-smoke-corrupt" <<'PY'
import pathlib
import struct
import sys

source = pathlib.Path(sys.argv[1])
destination = pathlib.Path(sys.argv[2])
data = bytearray(source.read_bytes())
descriptor_len = 16 + 4 + 4 + 8 * 4 + 32 * 4
descriptor = data[-descriptor_len:]
if descriptor[:16] != b"WITCHY-EXE-0092!":
    raise SystemExit("release-smoke: trusted executable has no RFC-0092 descriptor")
wasm_offset = struct.unpack_from("<Q", descriptor, 24)[0]
wasm_len = struct.unpack_from("<Q", descriptor, 32)[0]
if wasm_len < 16 or wasm_offset + wasm_len > len(data) - descriptor_len:
    raise SystemExit("release-smoke: trusted executable has invalid WASM bounds")
data[wasm_offset + 8] ^= 1
destination.write_bytes(data)
destination.chmod(source.stat().st_mode & 0o777)
PY
corrupt="$work/installed/trusted-smoke-corrupt"
if (cd run-root && PATH='' "$corrupt" inside.txt >corrupt.out 2>corrupt.err); then
    echo "release-smoke: corrupted trusted executable started successfully" >&2
    exit 1
fi
[ ! -s run-root/corrupt.out ] || {
    echo "release-smoke: corrupted trusted executable entered main" >&2
    exit 1
}
case "$(<run-root/corrupt.err)" in
    *'digest mismatch'*) ;;
    *) echo "release-smoke: corrupted trusted executable failed for the wrong reason" >&2; exit 1 ;;
esac

echo "release-smoke: PASS $target $commit"
