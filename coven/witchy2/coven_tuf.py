"""Authorize and resolve Coven targets."""
import contextlib
import hashlib
import os
from pathlib import Path
import stat
import tempfile
from urllib.parse import urlsplit

from coven_approval import HOST
from coven_https_client import downloader
from coven_permissions import extended_acl, lock_exclusive
from coven_staging import canonical, read_file, read_json
from coven_target import TRUST_BYTES, write_new
from coven_tuf_extract import extractor_worker
from coven_tuf_refresh import refresh
from coven_tuf_select import selector_worker
from witchy_release import DIGEST, NAME, VERSION, digest_of

ROOT_BYTES = 512000
METADATA_BYTES = 2 << 20
CACHE_LOCK_SECONDS = 30
TUF_WORKERS = {"tuf_fetch_select", "tuf_get", "tuf_get_loopback", "tuf_extract"}


def authorize(args, prepare, signer, finalize):
    from coven_tuf_sign import staged_delegation

    trust = read_file(args.trust, TRUST_BYTES)
    parent = read_file(args.parent, METADATA_BYTES)
    pins = {"role": args.role, "parent_pin": args.parent_pin, "trust_pin": args.trust_pin}
    with staged_delegation(parent, trust, args.bundles) as snapshot:
        unsigned = prepare(pins | {"version": args.version, "valid_days": args.valid_days}, snapshot)
        signatures = signer(args.key, pins, unsigned, snapshot)
    output = finalize({"role": args.role, "parent_pin": args.parent_pin}, parent, unsigned, signatures)
    write_new(args.out, output)


def origin(url):
    parsed = urlsplit(url)
    host, port = parsed.hostname, parsed.port
    canonical_url = f"{parsed.scheme}://{host}" + (f":{port}" if port else "")
    if (url != canonical_url or not host or len(host) > 253 or not HOST.fullmatch(host)
            or parsed.scheme == "https" and port == 443
            or parsed.scheme != "https" and (parsed.scheme != "http" or host != "127.0.0.1" or not port)):
        raise ValueError("TUF needs a canonical HTTPS origin or an explicit IPv4 loopback test port")
    return parsed


@contextlib.contextmanager
def cache(url, root_digest):
    floor = Path(os.environ.get("XDG_STATE_HOME") or Path.home() / ".local/state") / "coven" / "tuf"
    folder = floor / hashlib.sha256(canonical([url, root_digest])).hexdigest()
    folder.mkdir(mode=0o700, parents=True, exist_ok=True)
    fd = os.open(folder, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        mode = os.fstat(fd)
        if mode.st_uid != os.getuid() or mode.st_mode & 0o077 or extended_acl(fd):
            raise ValueError("TUF state requires an owner-only directory")
        try:
            lock_exclusive(fd, CACHE_LOCK_SECONDS)
        except TimeoutError:
            raise ValueError("TUF rollback state is busy. Retry when the current command finishes.") from None
        yield fd
        try:
            current = folder.stat(follow_symlinks=False)
        except FileNotFoundError:
            raise ValueError("TUF rollback state directory changed during verification") from None
        if not os.path.samestat(mode, current):
            raise ValueError("TUF rollback state directory changed during verification")
    finally:
        os.close(fd)


def provisioned_mirror(args):
    url = origin(args.registry)
    if args.ca_file is not None and url.scheme != "https":
        raise ValueError("--ca-file requires an HTTPS TUF origin")
    if args.ca_pin is not None and url.scheme != "https":
        raise ValueError("--ca-pin requires an HTTPS TUF origin")
    if args.ca_pin is not None and not DIGEST.fullmatch(args.ca_pin):
        raise ValueError("invalid HTTPS CA bundle SHA-384 pin")
    root = read_file(args.root, ROOT_BYTES)
    if not DIGEST.fullmatch(args.root_pin) or digest_of(root) != args.root_pin:
        raise ValueError("initial TUF root does not match its pin")
    trust = read_file(args.trust, TRUST_BYTES)
    if not DIGEST.fullmatch(args.trust_pin) or digest_of(trust) != args.trust_pin:
        raise ValueError("release trust file does not match its pin")
    return url, root, trust


def bundled_worker_pins(url, roles):
    folder = Path(__file__).resolve().with_name("tuf-workers")
    try:
        fd = os.open(folder, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    except FileNotFoundError:
        return None
    except OSError as error:
        raise ValueError("invalid bundled TUF worker directory") from error
    try:
        info = os.fstat(fd)
        if info.st_uid != os.getuid() or info.st_mode & 0o022 or extended_acl(fd):
            raise ValueError("invalid bundled TUF worker directory")
    finally:
        os.close(fd)
    try:
        pins = read_json(read_file(folder / "pins.json", 4096, owner_writes_only=True))
    except (OSError, ValueError) as error:
        raise ValueError("invalid bundled TUF worker pins") from error
    if (not isinstance(pins, dict) or pins.keys() != TUF_WORKERS
            or not all(isinstance(pin, str) and DIGEST.fullmatch(pin) for pin in pins.values())):
        raise ValueError("invalid bundled TUF worker pins")
    names = {"selector": "tuf_fetch_select", "extractor": "tuf_extract",
             "download": "tuf_get" if url.scheme == "https" else "tuf_get_loopback"}
    return {role: {"source": folder / f"{names[role]}.wasm", "pin": pins[names[role]]} for role in roles}


def mirror_workers(args, url, fetching):
    roles = ["selector", "download"]
    if fetching:
        roles.append("extractor")
    configured = {role: {"source": getattr(args, f"{role}_module"), "pin": getattr(args, f"{role}_pin")}
                  for role in roles}
    pins = [value for worker in configured.values() for value in worker.values()]
    if any(value is not None for value in pins):
        if any(value is None for value in pins):
            raise ValueError(f"supply the module and independent pin for each worker: {', '.join(roles)}")
    else:
        bundled = bundled_worker_pins(url, roles)
        if bundled is not None:
            return bundled
    return configured


@contextlib.contextmanager
def mirror_session(args, build, host, coordinate):
    url, root, trust = provisioned_mirror(args)
    workers = mirror_workers(args, url, coordinate is not None)
    request = {"registry": args.id, "root": args.root_pin, "trust": args.trust_pin, "coordinate": coordinate}
    with contextlib.ExitStack() as stack:
        select = stack.enter_context(selector_worker(build, host, root, trust, request, **workers["selector"]))
        extractor = (stack.enter_context(extractor_worker(build, host, **workers["extractor"]))
                     if coordinate is not None else None)
        client = stack.enter_context(downloader(build, host, url, args.ca_file, ca_pin=args.ca_pin, **workers["download"]))
        yield select, client, extractor, root, trust, request


def catalog(args, build, host):
    expected = {"snapshot": args.expect_snapshot, "timestamp": args.expect_timestamp}
    for field, pin in expected.items():
        if pin is not None and not DIGEST.fullmatch(pin):
            raise ValueError(f"invalid expected TUF {field} digest")
    with (mirror_session(args, build, host, None) as (select, client, _, root, _, request),
          cache(args.registry, args.root_pin) as folder):
        verified = refresh(select, folder, root, request, client)
        if client.ca_pin is not None:
            verified["ca_pin"] = client.ca_pin
    for field, pin in expected.items():
        if pin is not None and verified[field] != pin:
            raise ValueError(f"verified TUF {field} differs from the expected digest")
    return verified


def fetch(args, build, host):
    if not NAME.fullmatch(args.name) or not VERSION.fullmatch(args.version):
        raise ValueError("invalid Coven target coordinate")
    if args.expect is not None and not DIGEST.fullmatch(args.expect):
        raise ValueError("invalid locked package digest")
    if args.expect_snapshot is not None and not DIGEST.fullmatch(args.expect_snapshot):
        raise ValueError("invalid expected TUF snapshot digest")
    if args.expect_target is not None and not DIGEST.fullmatch(args.expect_target):
        raise ValueError("invalid locked TUF target digest")
    with tempfile.TemporaryDirectory(prefix="coven-target-") as directory:
        target_file = Path(directory).resolve() / "target"
        coordinate = f"{args.name}/{args.version}.coven"
        with mirror_session(args, build, host, coordinate) as (select, client, extractor, root, trust, request):
            with cache(args.registry, args.root_pin) as folder:
                selected = refresh(select, folder, root, request, client)
                if selected is None:
                    raise ValueError("package version is absent from verified TUF targets")
                snapshot, target, length, route, version = selected
                if args.expect_snapshot is not None and snapshot != args.expect_snapshot:
                    raise ValueError("verified TUF snapshot differs from the expected digest")
                if args.expect_target is not None and target != args.expect_target:
                    raise ValueError("verified TUF target differs from the locked digest")
                client.fetch(route, target_file, length=length)
            package, source, publisher, _, _ = extractor.extract(directory, {
                "name": args.name, "version": args.version, "registry": args.id, "trust": args.trust_pin,
                "target": target, "source": args.expect or ""}, trust)
    write_new(args.out, package)
    receipt = {"digest": source, "publisher": publisher, "snapshot": snapshot,
               "snapshot_version": version, "target": target}
    if client.ca_pin is not None:
        receipt["ca_pin"] = client.ca_pin
    return receipt
