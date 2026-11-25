"""Load one complete Glamour generation for Coven's read-only web routes."""
from base64 import b64encode
from contextlib import ExitStack
import fcntl
from hashlib import sha384
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import stat

NAME = re.compile(r"[a-z][a-z0-9-]{0,63}")
VERSION = re.compile(r"(?:0|[1-9][0-9]{0,8})(?:\.(?:0|[1-9][0-9]{0,8})){2}")
REGISTRY_ID = re.compile(r"[a-z0-9][a-z0-9.-]{0,63}")
DIGEST = re.compile(r"sha384:[0-9a-f]{96}")

HTML = "text/html; charset=utf-8"
SCRIPT = "text/javascript; charset=utf-8"
BASE_POLICY = ("default-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'; "
               "require-trusted-types-for 'script'")
NO_AMBIENT = BASE_POLICY + "; trusted-types 'none'"
STATIC_PAGE = NO_AMBIENT + "; style-src 'self'"
ISLAND_PAGE = (BASE_POLICY + "; trusted-types glamour-worker; script-src 'sha384-{bootstrap}'; "
               "worker-src 'self'; connect-src 'self'; style-src 'self'")
ISLAND_WORKER = NO_AMBIENT + "; script-src 'wasm-unsafe-eval'"
FRAMED_PAGE = (BASE_POLICY + "; trusted-types 'none'; script-src 'sha384-{bootstrap}'; "
               "worker-src 'none'; frame-src 'self'; connect-src 'self'; style-src 'self'")
FRAME_POLICY = ("default-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'self'; "
                "require-trusted-types-for 'script'; trusted-types glamour-worker; sandbox allow-scripts; "
                "script-src 'sha384-B+1nAHHPkrmlMHfJ9cwPpQdd8aEvaBBZQeMIlK74qfiyg6u2PXpddB0d0Ag1qmt4' "
                "'wasm-unsafe-eval'; worker-src blob:")
# The receipt cannot authorize changes to the browser code that enforces isolation.
SCRIPT_ASSET_PINS = {
    "glamour-browser.mjs": "sha384:f0b355c6c76d69b565c8cde73739635bd0e14f25af043be57ea1ad755a227ca93e4c0eda01fe96148d480a2a5fef933d",
    "glamour-worker.mjs": "sha384:c0eb76dbda49864a0911b661fcc69925903f6980f23a1204c483580aa949065d6bd10bccbd19762e8a3e167be68b6a7a",
    "glamour-frame.html": "sha384:30cba3fb89b2722bd403f238858acc683c8930bf9a8b219dc4f9f01f3fe5d00040403a8340f1f349d294d6ff63ea3e33",
}
ASSETS = {
    "index.html": ("/", HTML, ISLAND_PAGE, 49152),
    "coven.css": ("/coven.css", "text/css; charset=utf-8", NO_AMBIENT, 16384),
    "filter.wasm": ("/filter.wasm", "application/wasm", NO_AMBIENT, 65536),
    "glamour-browser.mjs": ("/glamour-browser.mjs", SCRIPT, NO_AMBIENT, 16384),
    "glamour-worker.mjs": ("/glamour-worker.mjs", SCRIPT, ISLAND_WORKER, 16384),
}
SOURCES = ("render.wy", "render.grants.toml", "browser/filter.wy", "browser/filter.grants.toml")


def init_project(project, registry, operator, registry_id, publisher, toml_string):
    """Copy the first-party app and bind its consumer manifests to explicit trust."""
    common = (f'\n[registry]\nurl = {toml_string(registry)}\noperator = {toml_string(operator)}\n'
              f'id = {toml_string(registry_id)}\nowners-required = true\n'
              '\n[dependencies]\nglamour = "0.3.0"\n'
              f'\n[owners]\nglamour = {toml_string(publisher)}\n')
    write_project(project, common, common, {})


def init_tuf_project(args, toml_string):
    from coven_https_client import read_trust_anchors
    from coven_tuf import provisioned_mirror

    _, root, trust = provisioned_mirror(args)
    files = {"registry-root.json": root, "release-trust.json": trust}
    fields = {"protocol": "tuf", "url": args.registry, "id": args.id,
              "root-pin": args.root_pin, "trust-pin": args.trust_pin}
    paths = {"root": "registry-root.json", "trust": "release-trust.json"}
    if args.ca_file is not None:
        files["registry-ca.pem"] = read_trust_anchors(args.ca_file, args.ca_pin)
        paths["ca-file"] = "registry-ca.pem"
    if args.ca_pin is not None:
        fields["ca-pin"] = args.ca_pin

    def registry(prefix):
        table = fields | {key: prefix + name for key, name in paths.items()}
        return "\n[registry]\n" + "".join(f"{key} = {toml_string(value)}\n" for key, value in table.items()) + (
            '\n[dependencies]\nglamour = "0.3.0"\n')

    write_project(args.project, registry(""), registry("../"), files)


def write_project(project, renderer_registry, browser_registry, files):
    template = Path(__file__).resolve().with_name("web")
    sources = {name: (template / name).read_bytes() for name in SOURCES}
    sources.update(files)
    renderer = ('[run]\nentry = "render.wy"\n'
                '\n[grants]\ndocument = "render.grants.toml"\noutput = true\nallocator = true\n'
                '\n[web]\nbrowser = "browser"\n')
    browser = ('[run]\nentry = "filter.wy"\n'
               '\n[grants]\ndocument = "filter.grants.toml"\noutput = true\n')
    sources["witchy.toml"] = (renderer + renderer_registry).encode("utf-8")
    sources["browser/witchy.toml"] = (browser + browser_registry).encode("utf-8")
    project.mkdir(mode=0o700)
    try:
        (project / "browser").mkdir()
        for name, data in sources.items():
            (project / name).write_bytes(data)
    except OSError:
        shutil.rmtree(project)
        raise


def matches(pattern, value):
    return isinstance(value, str) and pattern.fullmatch(value) is not None


def unique(pairs):
    result = dict(pairs)
    if len(result) != len(pairs):
        raise ValueError("duplicate Coven Web receipt field")
    return result


def read_file(folder, relative, limit):
    """Only bounded regular files under the already-open generation."""
    *parents, file = PurePosixPath(relative).parts
    with ExitStack() as opened:
        for part in parents:
            folder = os.open(part, os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=folder)
            opened.callback(os.close, folder)
            if os.fstat(folder).st_mode & 0o022:
                raise ValueError(f"writable Coven Web asset directory for {relative}")
        fd = os.open(file, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=folder)
        with os.fdopen(fd, "rb") as source:
            info = os.fstat(fd)
            if (not stat.S_ISREG(info.st_mode) or info.st_nlink != 1
                    or info.st_mode & 0o022 or info.st_size > limit):
                raise ValueError(f"invalid Coven Web asset {relative}")
            data = source.read(limit + 1)
    if len(data) > limit:
        raise ValueError(f"Coven Web asset {relative} exceeds its size limit")
    return data


def files_from(receipt):
    if not isinstance(receipt, dict) or set(receipt) != {"format", "catalog", "locks", "assets"}:
        raise ValueError("invalid Coven Web receipt format")
    profile = receipt["format"]
    bound = profile in ("coven-web-6", "coven-web-7")
    tuf = bound or receipt["format"] == "coven-web-5"
    if profile not in ("coven-web-4", "coven-web-8") and not tuf:
        raise ValueError("unsupported Coven Web generation: rebuild with current grimoire web build")
    locks = receipt["locks"]
    if (not isinstance(locks, dict) or set(locks) != {"renderer", "browser"}
            or not all(matches(DIGEST, value) for value in locks.values())):
        raise ValueError("invalid Coven Web lock digests")
    catalog = receipt["catalog"]
    fields = {"registry", "snapshot", "releases"} if tuf else {"registry", "sequence", "snapshot", "releases"}
    if bound:
        fields |= {"snapshot_version", "timestamp"}
    if (not isinstance(catalog, dict) or set(catalog) != fields
            or not matches(REGISTRY_ID, catalog["registry"])
            or not tuf and (type(catalog["sequence"]) is not int or catalog["sequence"] < 1)
            or not matches(DIGEST, catalog["snapshot"])
            or not isinstance(catalog["releases"], list) or len(catalog["releases"]) > 64):
        raise ValueError("invalid Coven Web catalog receipt")
    if bound and (type(catalog["snapshot_version"]) is not int or not 1 <= catalog["snapshot_version"] < 2**53
                  or not matches(DIGEST, catalog["timestamp"])):
        raise ValueError("invalid Coven Web TUF publication binding")
    files = dict(ASSETS)
    if profile in ("coven-web-7", "coven-web-8"):
        files["index.html"] = ("/", HTML, FRAMED_PAGE, 49152)
        files["glamour-frame.html"] = ("/glamour-frame.html", HTML, FRAME_POLICY, 16384)
    release_fields = {"name", "version", "target", "length"} if tuf else {"name", "version", "digest"}
    digest_field = "target" if tuf else "digest"
    for release in catalog["releases"]:
        if (not isinstance(release, dict) or set(release) != release_fields
                or not matches(NAME, release["name"]) or not matches(VERSION, release["version"])
                or not matches(DIGEST, release[digest_field])
                or tuf and (type(release["length"]) is not int or not 1 <= release["length"] <= (2 << 20))):
            raise ValueError("invalid Coven Web release coordinate")
        path = f"packages/{release['name']}/{release['version']}/"
        file = path + "index.html"
        if file in files:
            raise ValueError("duplicate Coven Web release")
        files[file] = ("/" + path, HTML, STATIC_PAGE, 8192)
    return files


def load(directory):
    return read_generation(directory)[1]


def read_generation(directory):
    """Verify the receipt and freeze bytes before opening the HTTP socket.

    The operator supplies the trusted, already-built directory. The receipt
    detects partial or mixed generations. No registry or compiler is contacted.
    """
    folder = os.open(directory, os.O_DIRECTORY)
    try:
        if os.fstat(folder).st_mode & 0o022:
            raise ValueError("Coven Web generation must not be writable by other accounts")
        try:
            receipt = json.loads(read_file(folder, "manifest.json", 65536), object_pairs_hook=unique)
        except (UnicodeError, RecursionError) as error:
            raise ValueError("invalid Coven Web receipt") from error
        files = files_from(receipt)
        digests = receipt["assets"]
        if not isinstance(digests, dict) or digests.keys() != files.keys():
            raise ValueError("Coven Web assets disagree with the catalog")
        routes = {}
        for file, (route, content_type, policy, limit) in files.items():
            data = read_file(folder, file, limit)
            digest = "sha384:" + sha384(data).hexdigest()
            if digests[file] != digest or SCRIPT_ASSET_PINS.get(file, digest) != digest:
                raise ValueError(f"Coven Web asset {file} failed digest verification")
            routes[route] = (data, content_type, policy)
        bootstrap = b64encode(sha384(routes["/glamour-browser.mjs"][0]).digest()).decode("ascii")
        page, content_type, policy = routes["/"]
        routes["/"] = (page, content_type, policy.format(bootstrap=bootstrap))
        return receipt, routes
    finally:
        os.close(folder)


def require_publication(held, device, catalog):
    from coven_tuf_publish import OBJECT_BYTES, protected_file, subdirectory

    try:
        with subdirectory(held, "metadata", device) as parent:
            for file, digest, limit in (
                ("timestamp.json", catalog["timestamp"], 16384),
                (f"{catalog['snapshot_version']}.snapshot.json", catalog["snapshot"], OBJECT_BYTES),
            ):
                with os.fdopen(protected_file(parent, file, device), "rb") as source:
                    body = source.read(limit + 1)
                if len(body) > limit or "sha384:" + sha384(body).hexdigest() != digest:
                    raise ValueError("Coven Web generation differs from the current mirror publication")
    except FileNotFoundError:
        raise ValueError("Coven Web generation names metadata missing from the mirror") from None


def install_mirror(generation, mirror):
    """Publish the complete route table only after its content-addressed bodies."""
    from coven_tuf_publish import Publication, check_owner, directory

    receipt, routes = read_generation(generation)
    if receipt["format"] not in ("coven-web-6", "coven-web-7"):
        raise ValueError("rebuild the TUF-backed Coven Web generation before installing on HTTPS")
    catalog = receipt["catalog"]
    profile = 4 if receipt["format"] == "coven-web-7" else 2
    timestamp = catalog["timestamp"].removeprefix("sha384:")
    table = f"coven-web-https-{profile}\n{timestamp}\n".encode("ascii")
    rows = []
    with directory(Path(os.path.abspath(mirror))) as held:
        device = check_owner(held, "mirror").st_dev
        fcntl.flock(held, fcntl.LOCK_EX)
        require_publication(held, device, catalog)
        publisher = Publication(held, device)
        for route, (data, _, _) in sorted(routes.items()):
            digest = sha384(data).hexdigest()
            publisher.install_bytes(Path("web/assets") / digest, data)
            rows.append(f"{route} {digest}\n")
        body = "".join(rows).encode("ascii")
        if profile == 4:
            table += (sha384(body).hexdigest() + "\n").encode("ascii")
        publisher.install_bytes(Path("web/routes"), table + body, replace=True)
