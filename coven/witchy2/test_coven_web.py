"""The static generation loader and the real loopback route boundary."""
from base64 import b64encode
from hashlib import sha384
from http.client import HTTPConnection
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

from cli_test_support import protocol_http_server
from coven_https import notify_ready, run_listener
from coven_mirror_service import check_identity, serve as serve_https_mirror
from coven_tuf_publish import Publication
from coven_web import ASSETS, install_mirror, load
from test_http_store import COVEN, raw_exchange, start_server


class WebGeneration(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.web = self.root / "web"
        self.web.mkdir()
        digest = "sha384:" + "0" * 96
        self.files = dict.fromkeys(ASSETS, b"fixture")
        self.site = Path(__file__).resolve().parents[2] / "grimoire"
        for name in ("glamour-browser.mjs", "glamour-worker.mjs"):
            self.files[name] = (self.site / name).read_bytes()
        self.files["packages/glamour/0.2.0/index.html"] = b"package page"
        self.receipt = {"format": "coven-web-4",
                        "catalog": {"registry": "test", "sequence": 2, "snapshot": digest,
                                    "releases": [{"name": "glamour", "version": "0.2.0", "digest": digest}]},
                        "locks": {"renderer": digest, "browser": digest},
                        "assets": {name: "sha384:" + sha384(data).hexdigest() for name, data in self.files.items()}}
        for name, data in self.files.items():
            path = self.web / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)
        self.write_receipt()

    def write_receipt(self):
        (self.web / "manifest.json").write_text(json.dumps(self.receipt))

    def test_https_mirror_refuses_a_non_loopback_bind(self):
        args = SimpleNamespace(listen="0.0.0.0:8419")
        with self.assertRaisesRegex(ValueError, "listener requires"):
            serve_https_mirror(args, "unreachable-host")
        args.listen = "127.0.0.1:65536"
        with self.assertRaisesRegex(ValueError, "listener requires"):
            serve_https_mirror(args, "unreachable-host")

    def test_private_https_rejects_public_binds_before_loading_identity(self):
        from coven_ci_service import serve as serve_ci
        from coven_release_http import serve as serve_review

        for serve in (serve_ci, serve_review):
            with self.assertRaisesRegex(ValueError, "listener requires"):
                serve(SimpleNamespace(listen="0.0.0.0:8443"), "unreachable-host")

    def test_private_listener_refuses_a_changed_module_before_readiness(self):
        host = shutil.which(os.environ.get("WITCHY_WASMTIME_HOST", "witchy-wasmtime-host"))
        if host is None:
            self.skipTest("install the Witchy Wasmtime host")
        module = self.root / "listener.wasm"
        pin = "sha384:" + sha384(b"original module").hexdigest()
        module.write_bytes(b"changed module")
        key = self.root / "tls.der"
        key.touch(mode=0o600)
        # The host must refuse the module pin before it tries to decode the TLS key.
        key.write_bytes(b"unused")
        destination = str(self.root / "notify.sock")
        spawn = subprocess.Popen
        with (socket.socket(socket.AF_UNIX, socket.SOCK_DGRAM) as ready,
              tempfile.TemporaryFile() as errors):
            ready.bind(destination)
            with (patch.dict(os.environ, {"NOTIFY_SOCKET": destination}),
                  patch.object(subprocess, "Popen", side_effect=lambda *a, **kw: spawn(*a, stderr=errors, **kw))):
                with self.assertRaisesRegex(ValueError, "did not become ready"):
                    run_listener(host, module, "127.0.0.1:8443", key, self.root,
                                 lambda request: self.fail("refused module reached the action handler"),
                                 1024, 1024, (), module_pin=pin)
            errors.seek(0)
            self.assertIn(b"module differs from its independent pin", errors.read())
            ready.setblocking(False)
            with self.assertRaises(BlockingIOError):
                ready.recv(32)

    def test_mirror_ca_pin_requires_an_exact_worker_receipt(self):
        ca = self.root / "ca.pem"
        ca.write_bytes(b"fixture")
        digest = sha384(ca.read_bytes())
        expected = "sha384:" + digest.hexdigest()
        args = ("host", ca, "module", "module-pin", self.root, self.root.stat(), 3)
        with patch("coven_mirror_service.run_with_lifeline") as run:
            run.return_value = subprocess.CompletedProcess([], 0, b"verified\n", b"")
            check_identity(*args)
            with self.assertRaisesRegex(ValueError, "CA bundle differs from its independent pin"):
                check_identity(*args, ca_pin=expected)
            run.return_value.stdout += digest.digest()
            check_identity(*args, ca_pin=expected)
            run.return_value.stdout += b"x"
            with self.assertRaisesRegex(ValueError, "Witchy refused the installed mirror TLS"):
                check_identity(*args, ca_pin=expected)

    def test_service_readiness_relays_only_the_host_token(self):
        destination = str(self.root / "notify.sock")
        with socket.socket(socket.AF_UNIX, socket.SOCK_DGRAM) as ready:
            ready.bind(destination)
            ready.settimeout(0.25)

            def child(payload, status=None):
                source, writer = os.pipe()
                with os.fdopen(writer, "wb") as sink:
                    sink.write(payload)
                return SimpleNamespace(poll=lambda: status, stdout=os.fdopen(source, "rb"))

            notify_ready(child(b"READY\n"), destination)
            self.assertEqual(ready.recv(32), b"READY=1")
            read = os.read
            with patch("coven_https.os.read", side_effect=lambda fd, size: read(fd, min(2, size))):
                notify_ready(child(b"READY\n"), destination)
            self.assertEqual(ready.recv(32), b"READY=1")
            for payload, status in ((b"", None), (b"RE", None), (b"partial", None),
                                    (b"READY\nx", None), (b"READY\n", 1)):
                process = child(payload, status)
                with self.assertRaisesRegex(ValueError, "did not become ready"):
                    notify_ready(process, destination)
            ready.setblocking(False)
            with self.assertRaises(BlockingIOError):
                ready.recv(32)

    def test_partial_service_readiness_does_not_reset_the_deadline(self):
        source, writer = os.pipe()
        os.close(writer)
        process = SimpleNamespace(poll=lambda: None, stdout=os.fdopen(source, "rb"))
        with (patch("coven_https.selectors.DefaultSelector") as selector,
              patch("coven_https.os.read", return_value=b"RE"),
              patch("coven_https.time.monotonic", side_effect=(100, 101, 109)),
              patch("coven_https.socket.socket") as socket_factory):
            available = selector.return_value.__enter__.return_value.select
            available.side_effect = ([True], [])
            with self.assertRaisesRegex(ValueError, "did not become ready"):
                notify_ready(process, "/unused")
        self.assertEqual([item.args for item in available.call_args_list], [(9,), (1,)])
        socket_factory.assert_not_called()

    def test_tuf_generation_preserves_the_sealed_target_coordinates(self):
        self.receipt["format"] = "coven-web-6"
        catalog = self.receipt["catalog"]
        catalog.pop("sequence")
        release = catalog["releases"][0]
        release["target"] = release.pop("digest")
        release["length"] = 1234
        mirror = self.root.resolve() / "published"
        metadata = mirror / "metadata"
        metadata.mkdir(parents=True)
        timestamp = metadata / "timestamp.json"
        timestamp.write_bytes(b"timestamp")
        snapshot = metadata / "7.snapshot.json"
        snapshot.write_bytes(b"snapshot")
        catalog["timestamp"] = "sha384:" + sha384(timestamp.read_bytes()).hexdigest()
        catalog["snapshot"] = "sha384:" + sha384(snapshot.read_bytes()).hexdigest()
        catalog["snapshot_version"] = 7
        self.write_receipt()
        self.assertEqual(load(self.web)["/packages/glamour/0.2.0/"][0], b"package page")
        install_mirror(self.web, mirror)
        table = mirror / "web/routes"
        previous = table.read_bytes()
        self.assertTrue(previous.startswith(f"coven-web-https-2\n{sha384(timestamp.read_bytes()).hexdigest()}\n".encode()))
        digest = sha384(b"package page").hexdigest()
        self.assertIn(f"/packages/glamour/0.2.0/ {digest}\n".encode(), previous)
        self.assertEqual((mirror / "web/assets" / digest).read_bytes(), b"package page")
        index = self.web / "index.html"
        new_page = b"new catalog"
        index.write_bytes(new_page)
        self.receipt["assets"]["index.html"] = "sha384:" + sha384(new_page).hexdigest()
        self.write_receipt()

        install = Publication.install_bytes

        def interrupt(publisher, path, data, **options):
            if path == Path("web/routes"):
                raise OSError("simulated loss of catalog installer before commit")
            return install(publisher, path, data, **options)

        with patch.object(Publication, "install_bytes", interrupt):
            with self.assertRaisesRegex(OSError, "before commit"):
                install_mirror(self.web, mirror)
        self.assertEqual(table.read_bytes(), previous)
        self.assertEqual((mirror / "web/assets" / sha384(self.files["index.html"]).hexdigest()).read_bytes(),
                         self.files["index.html"])
        self.assertEqual((mirror / "web/assets" / sha384(new_page).hexdigest()).read_bytes(), new_page)
        install_mirror(self.web, mirror)
        previous = table.read_bytes()
        self.assertIn(f"/ {sha384(new_page).hexdigest()}\n".encode(), previous)

        release["length"] = True
        self.write_receipt()
        with self.assertRaisesRegex(ValueError, "invalid Coven Web release coordinate"):
            install_mirror(self.web, mirror)
        self.assertEqual(table.read_bytes(), previous)
        release["length"] = 1234
        self.write_receipt()
        for path in (timestamp, snapshot):
            original = path.read_bytes()
            path.write_bytes(original + b"x")
            with self.assertRaisesRegex(ValueError, "differs from the current mirror publication"):
                install_mirror(self.web, mirror)
            self.assertEqual(table.read_bytes(), previous)
            path.write_bytes(original)
        self.receipt["format"] = "coven-web-5"
        del catalog["timestamp"], catalog["snapshot_version"]
        self.write_receipt()
        self.assertEqual(load(self.web)["/packages/glamour/0.2.0/"][0], b"package page")
        with self.assertRaisesRegex(ValueError, "rebuild the TUF-backed Coven Web generation"):
            install_mirror(self.web, mirror)
        self.assertEqual(table.read_bytes(), previous)

    def test_framed_preview_accepts_only_the_shipped_scripts_and_frame(self):
        self.receipt["format"] = "coven-web-8"
        frame = (self.site / "glamour-frame.html").read_bytes()
        self.receipt["assets"]["glamour-frame.html"] = "sha384:" + sha384(frame).hexdigest()
        (self.web / "glamour-frame.html").write_bytes(frame)
        self.write_receipt()
        routes = load(self.web)
        self.assertIn("worker-src 'none'", routes["/"][2])
        self.assertIn("sandbox allow-scripts", routes["/glamour-frame.html"][2])
        for name in ("glamour-browser.mjs", "glamour-worker.mjs", "glamour-frame.html"):
            with self.subTest(asset=name):
                path = self.web / name
                original = path.read_bytes()
                changed = original + b"\n"
                self.receipt["assets"][name] = "sha384:" + sha384(changed).hexdigest()
                path.write_bytes(changed)
                self.write_receipt()
                with self.assertRaisesRegex(ValueError, f"{name} failed digest verification"):
                    load(self.web)
                path.write_bytes(original)
                self.receipt["assets"][name] = "sha384:" + sha384(original).hexdigest()

    def test_refuses_mixed_oversize_and_linked_assets(self):
        index = self.web / "index.html"
        original = index.read_bytes()
        index.write_bytes(b"changed across builds")
        with self.assertRaisesRegex(ValueError, "failed digest verification"):
            load(self.web)
        index.write_bytes(b"x" * 49153)
        with self.assertRaisesRegex(ValueError, "invalid Coven Web asset"):
            load(self.web)
        index.write_bytes(original)
        index.chmod(0o666)
        with self.assertRaisesRegex(ValueError, "invalid Coven Web asset"):
            load(self.web)
        other = self.root / "other"
        other.write_bytes(original)
        index.unlink()
        index.symlink_to(other)
        with self.assertRaises(OSError):
            load(self.web)
        index.unlink()
        os.link(other, index)
        with self.assertRaisesRegex(ValueError, "invalid Coven Web asset"):
            load(self.web)

    def test_rejects_routes_other_than_verified_coordinates_and_fixed_assets(self):
        self.receipt["assets"]["store/other"] = self.receipt["catalog"]["snapshot"]
        self.write_receipt()
        with self.assertRaisesRegex(ValueError, "assets disagree"):
            load(self.web)
        self.receipt["catalog"]["releases"][0]["name"] = ".."
        self.write_receipt()
        with self.assertRaisesRegex(ValueError, "invalid Coven Web release coordinate"):
            load(self.web)

    def test_requires_a_generation_built_for_trusted_types(self):
        self.receipt["format"] = "coven-web-2"
        self.write_receipt()
        with self.assertRaisesRegex(ValueError, "rebuild with current grimoire web build"):
            load(self.web)

    def test_serves_frozen_web_bytes_and_the_unchanged_v1_api(self):
        command = [str(COVEN), "serve", "--store", str(self.root / "store"), "--port", "0", "--web-dir", str(self.web)]
        url = start_server(self, protocol_http_server(command), self.root / "log")
        port = int(url.rsplit(":", 1)[1])
        (self.web / "index.html").write_bytes(b"a later local file edit")

        def request(target, method="GET", headers=None):
            connection = HTTPConnection("127.0.0.1", port, timeout=3)
            try:
                connection.request(method, target, headers=headers or {})
                response = connection.getresponse()
                return response.status, dict(response.headers), response.read()
            finally:
                connection.close()

        status, headers, body = request("/")
        self.assertEqual((status, body), (200, b"fixture"))
        policy = headers["Content-Security-Policy"]
        bootstrap = b64encode(sha384(self.files["glamour-browser.mjs"]).digest()).decode()
        self.assertIn("frame-ancestors 'none'", policy)
        self.assertIn("require-trusted-types-for 'script'", policy)
        self.assertIn("trusted-types glamour-worker", policy)
        self.assertIn("style-src 'self'", policy)
        self.assertEqual([part for part in policy.split("; ") if part.startswith("script-src ")],
                         [f"script-src 'sha384-{bootstrap}'"])
        self.assertEqual(headers["X-Content-Type-Options"], "nosniff")
        detail = request("/packages/glamour/0.2.0/")
        self.assertEqual(detail[2], b"package page")
        self.assertEqual(request("/coven.css")[1]["Content-Type"], "text/css; charset=utf-8")
        for (status, response_headers, _), expected in (
            (detail, 200),
            (request("/glamour-worker.mjs", headers={"Origin": url}), 200),
            (request("/v1/snapshot"), 200),
            (request("/missing"), 404),
            (request("/", method="PUT"), 501),
        ):
            self.assertEqual(status, expected)
            self.assertIn("trusted-types 'none'", response_headers["Content-Security-Policy"])
            self.assertIn("require-trusted-types-for 'script'", response_headers["Content-Security-Policy"])
        self.assertEqual(request("/v1/snapshot", headers={"Origin": url})[0], 400)
        self.assertEqual(request("/", headers={"Origin": "http://127.0.0.1:1"})[0], 400)
        self.assertEqual(request("/", method="POST")[0], 400)
        for alias in ("/index.html", "/manifest.json", "/?q=x", "/%69ndex.html", "/packages/glamour/0.2.0/../",
                      "/packages/glamour/0.2.0", "/packages/glamour/0.1.0/", "//filter.wasm"):
            with self.subTest(alias=alias):
                raw = f"GET {alias} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
                self.assertIn(b" 404 ", raw_exchange(port, raw.encode()).split(b"\r\n", 1)[0])
