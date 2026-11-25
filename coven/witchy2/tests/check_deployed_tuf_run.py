"""Check deployment inputs and a Grimoire run against an independent release inventory."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
from ssl import get_default_verify_paths
import stat
import tomllib


REGISTRY_FIELDS = {"url", "id", "root-pin", "trust-pin"}
DIGEST = re.compile(r"sha384:[0-9a-f]{96}")
TRUST_INPUT_BYTES = 512 * 1024
CONTROL_INPUT_BYTES = 32 << 20
SOURCE_BYTES = 1 << 20
MODULE_BYTES = 16 << 20


def require(condition, subject):
    if not condition:
        raise ValueError(f"deployed TUF run differs in {subject}")


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError(f"duplicate JSON key {key!r}")
        result[key] = value
    return result


def snapshot_at(expected, version):
    require(type(version) is int and 1 <= version < 2**53, "the TUF snapshot version")
    digest = expected["snapshots"].get(str(version))
    require(isinstance(digest, str) and DIGEST.fullmatch(digest), f"the trusted snapshot pin for version {version}")
    return {"snapshot": digest, "snapshot-version": version}


def check_toolchain(expected, lock):
    require(expected.keys() == {"compiler", "host", "host-libraries"}, "the trusted toolchain fields")
    tools = {name: expected[name] for name in ("compiler", "host")}
    libraries = expected["host-libraries"]
    require(isinstance(libraries, dict) and all(name and "/" not in name for name in libraries)
            and all(isinstance(pin, str) and DIGEST.fullmatch(pin)
                    for pin in (*tools.values(), *libraries.values())), "the trusted toolchain pins")
    locked = {"host-libraries": {}} | lock.get("toolchain", {})
    require(locked == expected, "the locked toolchain pins")
    return tools


def read_artifact(path, subject, limit=CONTROL_INPUT_BYTES):
    fd = os.open(path, os.O_RDONLY | os.O_NONBLOCK | os.O_CLOEXEC)
    with os.fdopen(fd, "rb") as source:
        before = os.fstat(source.fileno())
        require(stat.S_ISREG(before.st_mode), f"{subject} file type")
        require(before.st_size <= limit, f"{subject} size")
        wire = source.read(before.st_size + 1)
        after = os.fstat(source.fileno())
    require(len(wire) == before.st_size and after.st_ctime_ns == before.st_ctime_ns, f"{subject} file changed during reading")
    return wire


def configured_path(base, name, subject):
    require(isinstance(name, str) and name and "\0" not in name, f"the configured {subject} path")
    return base / name


def digest_of(wire):
    return "sha384:" + hashlib.sha384(wire).hexdigest()


def check_trust_file(project, name, pin, subject):
    path = configured_path(project, name, subject)
    wire = read_artifact(path, f"the provisioned {subject}", TRUST_INPUT_BYTES)
    require(digest_of(wire) == pin, f"the provisioned {subject} digest")


def check_registry(expected, project, settings):
    registry = expected["registry"]
    require(registry.keys() == REGISTRY_FIELDS | {"ca-pin"}, "the trusted registry identity fields and HTTPS CA pin")
    ca_pin = registry["ca-pin"]
    require(isinstance(ca_pin, str) and DIGEST.fullmatch(ca_pin), "the trusted HTTPS CA pin")
    identity = {field: registry[field] for field in REGISTRY_FIELDS}
    require(isinstance(identity["url"], str) and identity["url"].startswith("https://")
            and isinstance(identity["id"], str) and identity["id"]
            and all(isinstance(identity[key], str) and DIGEST.fullmatch(identity[key])
                    for key in ("root-pin", "trust-pin")), "the trusted HTTPS registry identity")
    require(settings.get("protocol") == "tuf"
            and all(settings.get(field) == value for field, value in identity.items()), "the manifest registry identity")
    check_trust_file(project, settings["root"], identity["root-pin"], "TUF root")
    check_trust_file(project, settings["trust"], identity["trust-pin"], "release-trust")
    require(settings.get("ca-pin") == ca_pin, "the manifest HTTPS CA pin")
    if "ca-file" in settings:
        check_trust_file(project, settings["ca-file"], ca_pin, "HTTPS CA bundle")
    return identity


def check_project_files(expected, project, manifest, policy_pin):
    entry = configured_path(project, manifest["run"]["entry"], "project entry")
    require(isinstance(policy_pin, str) and DIGEST.fullmatch(policy_pin), "the trusted capability policy pin")
    policy = read_artifact(entry.with_suffix(".policy"), "the local capability policy")
    require(digest_of(policy) == policy_pin, "the local capability policy digest")

    document = manifest.get("grants", {}).get("document")
    grant = b""
    if document is not None:
        path = configured_path(project, document, "grant document")
        grant = read_artifact(path, "the local grant document")
    require(digest_of(grant) == expected["grants"], "the local grant document digest")

    sources = expected["sources"]
    require(isinstance(sources, dict) and entry.name in sources, "the trusted project source inventory")
    for name, pin in sources.items():
        relative = Path(name)
        require(not relative.is_absolute() and ".." not in relative.parts
                and relative.as_posix() == name and relative.suffix == ".wy"
                and "\0" not in name, f"the trusted project source path {name!r}")
        wire = read_artifact(entry.parent / relative, f"the local project source {name!r}", SOURCE_BYTES)
        require(digest_of(wire) == pin, f"the local project source digest of {name!r}")
    return entry


def check(expected_file, project, snapshot_version, toolchain, policy_pin, *, built_module=None, preflight=False):
    expected = json.loads(read_artifact(expected_file, "the trusted inventory"), object_pairs_hook=unique_object)
    manifest = read_artifact(project / "witchy.toml", "the local project manifest")
    require(digest_of(manifest) == expected["manifest"], "the local project manifest digest")
    settings = tomllib.loads(manifest.decode("utf-8"))
    registry = check_registry(expected, project, settings["registry"])
    lock_bytes = read_artifact(project / "witchy.lock", "the local lock")
    lock = tomllib.loads(lock_bytes.decode("utf-8"))
    snapshot = snapshot_at(expected, snapshot_version)
    locked_version = lock["tuf"]["snapshot-version"]
    locked_snapshot = snapshot_at(expected, locked_version)
    require(locked_version <= snapshot_version, "the snapshot rollback floor")
    ca_pin = expected["registry"]["ca-pin"]
    require(lock["tuf"].get("ca-pin") == ca_pin, "the locked HTTPS CA bundle pin")
    require(lock["tuf"] == registry | locked_snapshot | {"ca-pin": ca_pin}, "the locked registry identity or TUF snapshot")
    tools = check_toolchain(toolchain, lock)
    entry = check_project_files(expected, project, settings, policy_pin)
    rows = {row["name"]: row for row in lock["package"]}
    require(len(rows) == len(lock["package"]), "the number of locked packages")
    require(rows.keys() == expected["packages"].keys(), "the locked package graph")
    for name, package in expected["packages"].items():
        for field in ("digest", "target", "publisher", "requires", "version"):
            require(rows[name][field] == package[field], f"the locked {field} of {name}")
    if preflight:
        if "ca-file" not in settings["registry"]:
            ca_file = get_default_verify_paths().cafile
            require(ca_file is not None, "the configured default HTTPS CA bundle path")
            check_trust_file(project, os.path.abspath(ca_file), ca_pin, "default HTTPS CA bundle")
        print("verified approved deployment inputs and lock before execution")
        return

    record = json.loads(read_artifact(project / "run.json", "the local run record"), object_pairs_hook=unique_object)
    expected_output = expected["stdout"].encode("utf-8")
    require(read_artifact(project / "run.stdout", "the program output", len(expected_output)) == expected_output,
            "the program output")
    require(record["schema"] == "grimoire-build-record 1", "the run record schema")
    require(record.get("completed") is True, "the successful execution receipt")
    module = record["module"]
    require(isinstance(module, str) and DIGEST.fullmatch(module)
            and [flag for flag in record["host_flags"] if flag.startswith("--module-pin=")] == [f"--module-pin={module}"],
            "the module digest checked at host admission")
    if built_module is not None:
        built = read_artifact(built_module, "the retained build artifact", MODULE_BYTES)
        require(digest_of(built) == module, "the retained build artifact digest")
    require(record["lock"] == digest_of(lock_bytes), "the recorded lock digest")
    require(record["toolchain"] == tools, "the recorded compiler and host pins")
    require(record["entry"] == entry.name, "the recorded entry name")
    require(record["policy"] == policy_pin, "the recorded capability policy digest")
    require(record["sources"] == expected["sources"], "the project source digests")
    observed_tuf = record["tuf"]
    require(observed_tuf.get("ca-pin") == ca_pin, "the recorded HTTPS CA bundle pin")
    require(observed_tuf == snapshot | {"ca-pin": ca_pin} and type(observed_tuf["snapshot-version"]) is int,
            "the recorded TUF snapshot")
    for field, description in (("manifest", "project manifest"), ("grants", "grant document")):
        pin = expected[field]
        require(isinstance(pin, str) and DIGEST.fullmatch(pin), f"the trusted {description} pin")
        require(record[field] == pin, f"the recorded {description} digest")
    require(record["packages"] == expected["packages"], "the run package graph or digests")
    print(f"verified local project inputs, provisioned trust files, output, toolchain pins, exact package digests, and TUF snapshot {snapshot_version}")
    if built_module is not None:
        print(f"verified retained build artifact matches the module admitted by Wasmtime ({module})")


if __name__ == "__main__":
    cli = argparse.ArgumentParser(description=__doc__)
    cli.add_argument("expected", type=Path, help="expected.json from the trusted release channel")
    cli.add_argument("project", type=Path, help="project containing witchy.lock, run.json and run.stdout")
    cli.add_argument("snapshot", type=int, help="expected authenticated run snapshot version")
    cli.add_argument("--toolchain", required=True, type=Path, help="independently provisioned toolchain pins for this client")
    cli.add_argument("--policy-pin", required=True, help="SHA-384 pin of the reviewed capability policy")
    phase = cli.add_mutually_exclusive_group()
    phase.add_argument("--built-module", type=Path, help="retained grimoire build output to compare with the run's module pin")
    phase.add_argument("--preflight", action="store_true", help="verify the approved inputs and lock before execution")
    args = cli.parse_args()
    try:
        tools = json.loads(read_artifact(args.toolchain, "the trusted toolchain inventory"), object_pairs_hook=unique_object)
        check(args.expected, args.project, args.snapshot, tools, args.policy_pin,
              built_module=args.built_module, preflight=args.preflight)
    except (OSError, ValueError, KeyError, TypeError, AttributeError) as exc:
        cli.exit(1, f"{cli.prog}: {exc}\n")
