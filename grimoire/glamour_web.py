"""Build a reviewed Witchy renderer and browser island into one static generation."""
import base64
from contextlib import ExitStack
import fcntl
import hashlib
from html import escape
import json
import os
from pathlib import Path
import re
import shutil
import stat
import subprocess
import tempfile
import time
import tomllib
from typing import NamedTuple

WEB_ASSETS = {
    "glamour-frame.html": "30cba3fb89b2722bd403f238858acc683c8930bf9a8b219dc4f9f01f3fe5d00040403a8340f1f349d294d6ff63ea3e33",
    "glamour-browser.mjs": "f0b355c6c76d69b565c8cde73739635bd0e14f25af043be57ea1ad755a227ca93e4c0eda01fe96148d480a2a5fef933d",
    "glamour-worker.mjs": "c0eb76dbda49864a0911b661fcc69925903f6980f23a1204c483580aa949065d6bd10bccbd19762e8a3e167be68b6a7a",
    "coven.css": "4638c5dfb22b5b35c2e1cd70b773bfde21de059294bdef01548cffe139d77054a29cf2ba3d74d5bba9a7b56aef41b534",
}
MODULE_BYTES = 65536
BASE_POLICY = "default-src 'none'; base-uri 'none'; form-action 'none'; require-trusted-types-for 'script'"


class ProjectInputs(NamedTuple):
    manifest: Path
    manifest_bytes: bytes
    grant: Path
    grant_bytes: bytes
    lock: Path
    lock_bytes: bytes

    def check(self, grimoire):
        for path, wire in ((self.manifest, self.manifest_bytes), (self.grant, self.grant_bytes),
                           (self.lock, self.lock_bytes)):
            if grimoire.read_control_file(path) != wire:
                raise ValueError(f"web build input changed: {path}")


def hex_digest(data):
    return hashlib.sha384(data).hexdigest()


def manifests(grimoire, project):
    renderer = (project / "witchy.toml" if project.is_dir() else project).resolve()
    wire = grimoire.read_control_file(renderer)
    config = tomllib.loads(wire.decode("utf-8"))
    web = config.get("web")
    if not isinstance(web, dict) or set(web) != {"browser"} or not isinstance(web["browser"], str):
        raise ValueError("[web] must name a browser project under browser")
    path = Path(web["browser"])
    browser = renderer.parent / path / "witchy.toml"
    if path.is_absolute() or not path.parts or ".." in path.parts or browser.resolve() != browser:
        raise ValueError("[web].browser must name a local project directory without links")
    return renderer, browser, wire


def session(grimoire, manifest, manifest_bytes, allocator):
    project = tomllib.loads(manifest_bytes.decode("utf-8"))
    run = project.get("run", {})
    _, document, grants = grimoire.read_project(manifest, project=project)
    grant_bytes = grimoire.read_control_file(document) if document else b""
    if not document or tomllib.loads(grant_bytes.decode("utf-8")) != {"input": {"input": {"stdin": True}}}:
        raise ValueError("web executables must declare only the stdin input")
    if grants.get("output") is not True or grants.get("allocator", False) is not allocator:
        raise ValueError("web renderer requires output and allocator; browser filter requires only output")
    if any(size in run for size in grimoire.BUILD_SIZES):
        raise ValueError("web executables use the compiler's bounded default memory layout")
    registry, dependencies = grimoire.read_dependencies(manifest, project=project)
    tuf = isinstance(registry, grimoire.TufRegistry)
    if "glamour" not in dependencies or not tuf and not (registry and registry.operator and registry.owners_required):
        raise ValueError("web executables require pinned registry trust and a locked Glamour dependency")
    lock = manifest.with_name("witchy.lock")
    lock_bytes = grimoire.read_lock_bytes(lock)
    if lock_bytes is None:
        raise ValueError("lock both web executables with grimoire lock --toolchain")
    if tuf:
        pins, snapshot, _ = grimoire.read_tuf_lock(lock, registry, wire=lock_bytes)
        identity = registry.identity()
    else:
        pins, snapshot = grimoire.parse_lock(lock, wire=lock_bytes), grimoire.read_lock_snapshot(lock, wire=lock_bytes)
        identity = (str(registry), registry.operator, registry.registry_id)
    if (not pins or "glamour" not in pins or pins["glamour"][0] != dependencies["glamour"]
            or not snapshot or not grimoire.read_toolchain(lock, wire=lock_bytes)):
        raise ValueError("lock both web executables with grimoire lock --toolchain")
    return registry, (identity, pins, snapshot), ProjectInputs(manifest, manifest_bytes, document, grant_bytes, lock, lock_bytes)


def run_guest(host, module, data, limits, timeout):
    descriptor, pin = module
    read_end, write_end = os.pipe()
    try:
        command = [host, f"--module-fd={descriptor}", f"--module-pin={pin}", f"--module-bytes={MODULE_BYTES}",
                   f"--lifeline=fd:{read_end}", "--input-stdin", "--output", "--memory-pages=4",
                   "--deadline-ms=1000", *limits]
        return subprocess.run(command, pass_fds=(descriptor, read_end), env={}, input=data,
                              stdout=subprocess.PIPE, check=True, timeout=timeout).stdout
    finally:
        os.close(write_end)
        os.close(read_end)


def render(host, module, mode, fields, output_limit, deadline):
    remaining = deadline - time.monotonic()
    if remaining <= 0:
        raise ValueError("web rendering exceeded the build deadline")
    data = (mode + "".join(f"{value}\n" for value in fields)).encode("utf-8")
    limits = ("--input-bytes=16384", "--allocator", f"--output-bytes={output_limit}", "--fuel=10000000")
    try:
        return run_guest(host, module, data, limits, min(5, remaining))
    except subprocess.TimeoutExpired as error:
        raise ValueError("web rendering exceeded the build deadline") from error


def shell(title, body, stylesheet, island=None):
    policy = BASE_POLICY + "; style-src 'self'; trusted-types 'none'"
    css_integrity = base64.b64encode(hashlib.sha384(stylesheet).digest()).decode("ascii")
    meta = script = ""
    page_class = "package-page"
    if island:
        page_class = "catalog-page"
        wasm, source, worker = island
        integrity = base64.b64encode(hashlib.sha384(source).digest()).decode("ascii")
        policy = (BASE_POLICY + f"; trusted-types 'none'; script-src 'sha384-{integrity}'; "
                  "worker-src 'none'; frame-src 'self'; connect-src 'self'; style-src 'self'")
        meta = (f'<meta name="glamour-island-sha384" content="{hex_digest(wasm)}">\n'
                f'<meta name="glamour-worker-sha384" content="{hex_digest(worker)}">\n')
        script = f'<script type="module" src="/glamour-browser.mjs" integrity="sha384-{integrity}"></script>'
    return (f'<!doctype html><html lang="en"><head><meta charset="utf-8"><title>{escape(title)}</title>\n'
            f'<meta http-equiv="Content-Security-Policy" content="{policy}">\n'
            f'{meta}'
            '<meta name="color-scheme" content="dark">\n'
            '<meta name="viewport" content="width=device-width, initial-scale=1">\n'
            f'<link rel="stylesheet" href="/coven.css" integrity="sha384-{css_integrity}">\n'
            '</head><body><header class="site-header"><div class="site-bar">'
            '<a class="wordmark" href="/"><span class="mark" aria-hidden="true"></span>Coven</a>'
            '</div></header>'
            f'<main class="{page_class}">{body}</main>\n{script}</body></html>\n').encode("utf-8")


def publish(root, assets, receipt, verify):
    dist = root / "dist"
    # A fixed public path points to a complete immutable generation. The
    # previous generation remains readable by a preview process holding it.
    if os.path.lexists(dist):
        if not dist.is_symlink() or not re.fullmatch(r"\.coven-web-[a-z0-9_]+", os.readlink(dist)):
            raise ValueError("dist exists and is not a Grimoire web generation")
    with tempfile.TemporaryDirectory(prefix=".coven-link-", dir=root) as temporary:
        generation = Path(tempfile.mkdtemp(prefix=".coven-web-", dir=root))
        pointer = Path(temporary) / "dist"
        try:
            for name, data in assets.items():
                path = generation / name
                path.parent.mkdir(mode=0o755, parents=True, exist_ok=True)
                for parent in path.parents:
                    if parent == generation:
                        break
                    parent.chmod(0o755)
                path.write_bytes(data)
                path.chmod(0o644)
            manifest = generation / "manifest.json"
            manifest.write_text(json.dumps(receipt, sort_keys=True) + "\n")
            manifest.chmod(0o644)
            verify(generation)
            generation.chmod(0o755)
            pointer.symlink_to(generation.name)
        except BaseException:
            shutil.rmtree(generation)
            raise
        try:
            os.replace(pointer, dist)
        except OSError:
            shutil.rmtree(generation)
            raise
    print(dist)


def build(grimoire, project):
    renderer, browser, renderer_bytes = manifests(grimoire, project)
    lock = renderer.with_name(".coven-web.lock")
    fd = os.open(lock, os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW | os.O_NONBLOCK, 0o600)
    try:
        if not stat.S_ISREG(os.fstat(fd).st_mode):
            raise ValueError("invalid web build lock")
        fcntl.flock(fd, fcntl.LOCK_EX)
        build_generation(grimoire, renderer, browser, renderer_bytes)
    finally:
        os.close(fd)


def build_generation(grimoire, renderer, browser, renderer_bytes):
    registry, registry_lock, renderer_inputs = session(grimoire, renderer, renderer_bytes, True)
    _, browser_lock, browser_inputs = session(grimoire, browser, grimoire.read_control_file(browser), False)
    tuf = isinstance(registry, grimoire.TufRegistry)
    if registry_lock != browser_lock:
        raise ValueError("web executables must use the same locked packages and registry snapshot")
    _, _, snapshot = registry_lock
    released, ca_pin = coven_catalog_web.catalog(grimoire, registry, snapshot)
    here = Path(__file__).resolve().parent
    assets = {}
    for name, pin in WEB_ASSETS.items():
        with (here / name).open("rb") as stream:
            data = stream.read(16385)
        if len(data) > 16384 or hex_digest(data) != pin:
            raise ValueError(f"{name} differs from the asset shipped with Grimoire")
        assets[name] = data
    with tempfile.TemporaryDirectory(prefix="grimoire-web-") as temporary, ExitStack() as guests:
        work = Path(temporary).resolve()
        renderer_wasm, filter_wasm = work / "renderer.wasm", work / "filter.wasm"
        catalog_snapshot = (released["snapshot"], released["snapshot_version"] if tuf else released["sequence"])
        grimoire.run_project(renderer, build_only=True, output=renderer_wasm,
                             catalog_snapshot=catalog_snapshot, catalog_ca_pin=ca_pin, captured_inputs=renderer_inputs)
        grimoire.run_project(browser, build_only=True, output=filter_wasm,
                             catalog_snapshot=catalog_snapshot, catalog_ca_pin=ca_pin, captured_inputs=browser_inputs)
        def open_guest(path):
            descriptor, wire = grimoire.open_compiled_module(path, MODULE_BYTES)
            guests.callback(os.close, descriptor)
            path.unlink()
            return (descriptor, "sha384:" + hex_digest(wire)), wire

        renderer_module, _ = open_guest(renderer_wasm)
        filter_module, wasm = open_guest(filter_wasm)
        host = grimoire.tool("WITCHY_WASMTIME_HOST", "witchy-wasmtime-host")
        limits = ("--input-bytes=4226", "--output-bytes=65", "--fuel=1000000")
        if run_guest(host, filter_module, b"\x00\x00", limits, 5) != b"\x01":
            raise ValueError("browser filter did not accept the empty catalog")
        assets["filter.wasm"] = wasm
        deadline = time.monotonic() + 20

        def execute(mode, fields, limit):
            return render(host, renderer_module, mode, fields, limit, deadline)

        for name, title, body in coven_catalog_web.render_pages(execute, released):
            island = (wasm, assets["glamour-browser.mjs"], assets["glamour-worker.mjs"]) if name == "index.html" else None
            assets[name] = shell(title, body, assets["coven.css"], island)
    if coven_catalog_web.catalog(grimoire, registry, snapshot, ca_pin=ca_pin) != (released, ca_pin):
        raise ValueError("verified catalog changed during the web build")
    lock_digests = {"renderer": "sha384:" + hex_digest(renderer_inputs.lock_bytes),
                    "browser": "sha384:" + hex_digest(browser_inputs.lock_bytes)}
    receipt = {"format": "coven-web-7" if tuf else "coven-web-8", "catalog": released,
               "locks": lock_digests,
               "assets": {name: "sha384:" + hex_digest(data) for name, data in assets.items()}}
    def verify(generation):
        checked = grimoire.run_coven([grimoire.tool("COVEN", "coven"), "web", "check", generation])
        grimoire.relay(checked.stderr)
        checked.check_returncode()
        renderer_inputs.check(grimoire)
        browser_inputs.check(grimoire)

    publish(renderer.parent, assets, receipt, verify)
