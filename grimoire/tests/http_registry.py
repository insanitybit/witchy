"""Signed publish, operator admission, restart and clean resolution through a real HTTP socket."""
import contextlib
from concurrent.futures import ThreadPoolExecutor
import hashlib
import os
from pathlib import Path
import select
import shlex
import shutil
import signal
import sqlite3
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]


class HttpRegistry(unittest.TestCase):
    def test_only_the_snapshotted_signed_release_reaches_a_clean_consumer(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            for role in ("operator", "author", "service", "reader", "builder", "reviewer", "refuser",
                         "wrong-key", "wrong-registry", "wrong-anchor"):
                (root / role).mkdir(mode=0o700)
            coven = os.environ.get("COVEN", "coven")

            # Check the environment received by each actual client tool without collecting it.
            seen = root / "tools"
            tools = {}
            for variable, default in (("WITCHYC", "witchyc"), ("COVEN", "coven"),
                                      ("WITCHY_WASMTIME_HOST", "witchy-wasmtime-host")):
                wrapper = root / variable
                real = shutil.which(os.environ.get(variable, default))
                self.assertIsNotNone(real, default)
                wrapper.write_text(
                    '#!/bin/sh\n'
                    'test -z "${REPO_TOKEN+x}${AWS_SECRET_ACCESS_KEY+x}${WITCHY_SECRET_X+x}" || exit 71\n'
                    f'echo {variable} >> {shlex.quote(str(seen))}\nexec {shlex.quote(real)} "$@"\n')
                wrapper.chmod(0o700)
                tools[variable] = str(wrapper)

            def environment(role):
                env = dict(os.environ, HOME=str(root / role), XDG_STATE_HOME=str(root / role / "state"))
                if role in ("reader", "builder", "reviewer", "refuser", "wrong-anchor"):
                    env.update(tools, REPO_TOKEN="canary-7f3a9", AWS_SECRET_ACCESS_KEY="canary-7f3a9",
                               WITCHY_SECRET_X="canary-7f3a9")
                return env

            def command(role, *arguments, success=True):
                result = subprocess.run(arguments, cwd=root / role, env=environment(role), capture_output=True,
                                        text=True, timeout=60)
                self.assertEqual(result.returncode == 0, success, result.stderr + result.stdout)
                self.assertNotIn("canary-7f3a9", result.stdout + result.stderr)
                return result.stdout if success else result.stdout + result.stderr

            def refused(role, message, *arguments):
                self.assertIn(message, command(role, ROOT / "grimoire", *arguments, success=False))

            @contextlib.contextmanager
            def service(port):
                proc = subprocess.Popen([coven, "serve", "--store", root / "store", "--port", str(port)],
                                        cwd=root / "service", env=environment("service"),
                                        stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
                try:
                    self.assertTrue(select.select([proc.stdout], [], [], 15)[0], "registry startup timed out")
                    ready = proc.stdout.readline().strip()
                    self.assertTrue(ready.startswith("Coven listening at http://127.0.0.1:"), ready)
                    yield ready.split()[-1]
                finally:
                    if proc.poll() is None:
                        proc.send_signal(signal.SIGINT)
                    try:
                        self.assertEqual(proc.wait(timeout=10), 0)
                    finally:
                        if proc.poll() is None:
                            proc.kill()
                            proc.wait()
                        proc.stdout.close()

            def make_key(role):
                key = root / role / "key"
                key.write_bytes(os.urandom(32))
                key.chmod(0o600)
                return command(role, coven, "key", "--key", "key").split()[1]

            key_roles = ("operator", "author")
            with ThreadPoolExecutor(max_workers=2) as pool:
                pins = dict(zip(key_roles, pool.map(make_key, key_roles)))
            library = root / "author/library"
            app = root / "reader/app"
            shutil.copytree(ROOT / "examples/local-run/config-reader", library)
            shutil.copytree(ROOT / "examples/local-run/app", app)
            (app / "main.packages").unlink()
            with service(0) as url:
                address = int(url.rpartition(":")[2])
                command("operator", coven, "snapshot", "--registry", root / "store", "--key", "key", "--first", "--id", "test")
                registry = f'\n[registry]\nurl = "{url}"\noperator = "{pins["operator"]}"\nid = "test"\n'
                with (library / "witchy.toml").open("a") as manifest:
                    manifest.write(registry)
                with (app / "witchy.toml").open("a") as manifest:
                    manifest.write(registry + f'\n[dependencies]\nconfig-reader = "0.1.0"\n[owners]\nconfig-reader = "{pins["author"]}"\n')
                database = root / "store" / "coven-registry.sqlite3"
                sentinel = ("config-reader-extra/unrelated", "not a registry BLOB")
                with contextlib.closing(sqlite3.connect(database)) as db, db:
                    db.execute("INSERT INTO objects VALUES (?, ?)", sentinel)
                result = command("author", ROOT / "grimoire", "publish", "library", "--key", "key")
                with contextlib.closing(sqlite3.connect(database)) as db, db:
                    self.assertEqual(db.execute("SELECT data FROM objects WHERE name=?", sentinel[:1]).fetchone(),
                                     (sentinel[1],))
                    db.execute("DELETE FROM objects WHERE name=?", sentinel[:1])
                expected = hashlib.sha384(b"witchy-package 1\nname config-reader\nversion 0.1.0\n\n" +
                                          (library / "reader.wy").read_bytes()).hexdigest()
                self.assertIn("sha384:" + expected, result)
                # A served registry's first claim of a name is signed only when the operator names it,
                # after reading the review; without `--admit`, nothing is signed.
                snapshot = ("operator", coven, "snapshot", "--registry", url, "--key", "key", "--min-base", "1", "--id", "test")
                review = command(*snapshot, success=False)
                self.assertIn(f"review: first claim: config-reader by {pins['author']}", review)
                served = ("operator", coven, "verify-snapshot", "--registry", url, "--operator", pins["operator"], "--registry-id", "test")
                self.assertTrue(command(*served).startswith("snapshot 1\n"))
                reviewed = command(*snapshot, "--admit", "config-reader")
                self.assertIn("review: first claim: config-reader", reviewed)
            shutil.rmtree(library)
            with service(address):
                # Verify the admitted snapshot survived restart, independently of the clean lock.
                with ThreadPoolExecutor(max_workers=2) as pool:
                    verified = pool.submit(command, *served)
                    self.assertIn(expected, command("reader", ROOT / "grimoire", "lock", "app"))
                    self.assertTrue(verified.result().startswith("snapshot 2\n"))
                self.assertIn('version = "0.1.0"', (app / "witchy.lock").read_text())
                # Both consumers start with the same lock and trust state.
                # The reader keeps its own project for the subsequent update.
                builder = root / "builder"
                shutil.copytree(app, builder / "app")
                shutil.copytree(root / "reader/state", builder / "state")

                def publish_update():
                    shutil.copytree(ROOT / "examples/local-run/config-reader", library)
                    reader = (library / "reader.wy").read_text()
                    self.assertIn("port > 65535", reader)
                    (library / "reader.wy").write_text('import "limits.wy"\n' + reader.replace("port > 65535", "port > limits.most()"))
                    (library / "limits.wy").write_text("pub fn most() -> U64:\n    return 65535\n")
                    (library / "witchy.toml").write_text((library / "witchy.toml").read_text().replace('"0.1.0"', '"0.1.1"') + registry)
                    published = command("author", ROOT / "grimoire", "publish", "library", "--key", "key")
                    pins_of = {name: "sha384:" + hashlib.sha384((library / name).read_bytes()).hexdigest() for name in ("limits.wy", "reader.wy")}
                    record = ("witchy-package 2\nname config-reader\nversion 0.1.1\nentry reader.wy\n"
                              + "".join(f"file {name} {(library / name).stat().st_size} {pins_of[name]}\n" for name in ("limits.wy", "reader.wy")) + "\n").encode()
                    release = "sha384:" + hashlib.sha384(record + (library / "limits.wy").read_bytes() + (library / "reader.wy").read_bytes()).hexdigest()
                    self.assertIn(f"config-reader 0.1.1: published {release}", published)
                    return pins_of, release

                # Publishing leaves the first signed snapshot in place while both consumers use it.
                # The operator signs the update only after both consumers finish.
                with ThreadPoolExecutor(max_workers=2) as pool:
                    pending = pool.submit(command, "builder", ROOT / "grimoire", "build", "app")
                    update = pool.submit(publish_update)
                    self.assertEqual(command("reader", ROOT / "grimoire", "run", "app"), "8080\n")
                    pending.result()
                    pins_of, release = update.result()
                self.assertEqual((builder / "app/target/main.wasm").read_bytes()[:4], b"\0asm")
                self.assertEqual(set(seen.read_text().splitlines()), set(tools))
                self.assertIn(f'[snapshot]\noperator = "{pins["operator"]}"\nsequence = 2\nregistry = "test"\n',
                              (app / "witchy.lock").read_text())
                # An update that splits the library over two files, through the same socket: the
                # release lists both, the consumer's build stops at the unreviewed file, the review
                # names it, and the accepted update runs.
                command("operator", coven, "snapshot", "--registry", url, "--key", "key", "--min-base", "2", "--id", "test")
                size = (library / "limits.wy").stat().st_size
                shutil.rmtree(library)
                manifest = app / "witchy.toml"
                manifest.write_text(manifest.read_text().replace('config-reader = "0.1.0"', 'config-reader = "0.1.1"'))
                self.assertIn(release, command("reader", ROOT / "grimoire", "lock", "app"))
                # Both unreviewed commands start with the reader's lock, policy and trust state.
                # They use private copies while the reader reviews the pinned update.
                for role in ("reviewer", "refuser"):
                    shutil.copytree(app, root / role / "app", ignore=shutil.ignore_patterns("target"))
                    shutil.copytree(root / "reader/state", root / role / "state")
                with ThreadPoolExecutor(max_workers=2) as pool:
                    pending = pool.submit(command, "reviewer", ROOT / "grimoire", "review", "app", success=False)
                    failure = pool.submit(refused, "refuser", "@config-reader/limits.wy is in no line of the policy", "run", "app")
                    with (app / "main.policy").open("a") as policy:
                        policy.write(f"@config-reader/limits.wy: nothing ; no unchecked ; digest {pins_of['limits.wy']}\n")
                    command("reader", ROOT / "grimoire", "review", "--accept", "app")
                    self.assertIn(pins_of["reader.wy"], (app / "main.policy").read_text())
                    failure.result()
                    review = pending.result()
                self.assertIn(f"@config-reader 0.1.1: 2 files in the release {release}", review)
                self.assertIn(f"  @config-reader/limits.wy: {size} bytes, {pins_of['limits.wy']}", review)
                self.assertIn("@config-reader/limits.wy 0.1.1: a new file (2 lines), in no line of the policy: read it whole, ", review)
                self.assertIn("@config-reader/limits.wy: 1 reachable functions; can use nothing", review)
                # The operator is checked twice, by Coven from the key Grimoire hands it and by
                # Grimoire's own check that every anchor is in the verified chain. Each is checked
                # here where the other passes, so neither hides the other's removal.
                # The lock pins a key outside the chain; witchy.toml declares the operator, the key
                # Coven is handed, so Coven accepts the snapshot: Grimoire's anchor check refuses.
                original_manifest = manifest.read_text()
                lock = app / "witchy.lock"
                locked = lock.read_text()
                self.assertEqual(locked.count(f'operator = "{pins["operator"]}"'), 1)
                wrong_anchor = root / "wrong-anchor"
                shutil.copytree(app, wrong_anchor / "app", ignore=shutil.ignore_patterns("target"))
                shutil.copytree(root / "reader/state", wrong_anchor / "state")
                (wrong_anchor / "app/witchy.lock").write_text(
                    locked.replace(f'operator = "{pins["operator"]}"', f'operator = "{pins["author"]}"'),
                )

                def refuses_fresh_lock(role, modified_manifest, diagnostic):
                    project = root / role / "app"
                    shutil.copytree(app, project, ignore=shutil.ignore_patterns("witchy.lock", "target"))
                    (project / "witchy.toml").write_text(modified_manifest)
                    refused(role, diagnostic, "lock", project)
                    self.assertFalse((project / "witchy.lock").exists())

                # Each refusal uses a private project and trust state while
                # the reader runs from its accepted lock.
                with ThreadPoolExecutor(max_workers=3) as pool:
                    wrong_key = pool.submit(
                        refuses_fresh_lock, "wrong-key", original_manifest.replace(pins["operator"], pins["author"]),
                        f"signed by {pins['operator']}, and its operator is {pins['author']}",
                    )
                    wrong_registry = pool.submit(
                        refuses_fresh_lock, "wrong-registry", original_manifest.replace('id = "test"', 'id = "other"'),
                        "the registry's snapshot is signed for registry test, not other",
                    )
                    anchor_refusal = pool.submit(
                        refused, "wrong-anchor", f"by an operator chain without {pins['author']}, which witchy.lock pins",
                        "run", "app",
                    )
                    self.assertEqual(command("reader", ROOT / "grimoire", "run", "app"), "8080\n")
                    self.assertEqual(lock.read_text(), locked)
                    wrong_key.result()
                    wrong_registry.result()
                    anchor_refusal.result()


if __name__ == "__main__":
    unittest.main()
