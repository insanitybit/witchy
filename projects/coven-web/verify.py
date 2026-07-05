#!/usr/bin/env python3
"""Self-contained e2e verification for coven-web (PLAN.md §9/§11).

Spawns a real coven registry + the coven-web server on throwaway ports, seeds a
published rune, and asserts the full security + functional contract:
  - Perfect Types CSP + strict COOP/COEP/CORP on every route class
  - correct MIME types; the sandbox-frame's own CSP (connect-src 'none')
  - the reverse proxy returns coven data; non-allowlisted /api paths 404 (anti-SSRF)
  - the Sec-Fetch CSRF layer: cross-site/same-site writes 403
  - the ONLY state-changing routes are the WebAuthn-2FA promote + yank; the old plain,
    session-only /api/coven/promote and /api/coven/yank routes are gone (404)
  - a real ES256 assertion drives promote-2fa AND yank-2fa end-to-end (200)
  - a publish -> /api/coven/source round-trip

Run from the repo root:  python3 projects/coven-web/verify.py
Exits non-zero on the first failed assertion.
"""
import json
import os
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import urllib.request
import urllib.error

REPO = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
# BUG-029: accept whichever witchy is available instead of assuming a debug build exists.
# Precedence: $WITCHY, then the release build, then debug, then whatever is on PATH (the
# installed release binary). The worktree may have no local target/ at all.
_dbg = os.path.join(REPO, "target", "debug", "witchy")
_rel = os.path.join(REPO, "target", "release", "witchy")
BIN = (os.environ.get("WITCHY")
       or (_rel if os.path.exists(_rel) else None)
       or (_dbg if os.path.exists(_dbg) else None)
       or shutil.which("witchy"))
if not BIN or not os.path.exists(BIN):
    print("verify.py: no witchy binary found (set $WITCHY, build target/{release,debug}/witchy, "
          "or install witchy on PATH)")
    sys.exit(2)
COVEN = "127.0.0.1:18799"
WEB = "127.0.0.1:18080"
WEB_URL = "http://" + WEB
COVEN_URL = "http://" + COVEN

failures = []


def check(name, cond, detail=""):
    status = "PASS" if cond else "FAIL"
    print(f"[{status}] {name}" + (f" — {detail}" if detail and not cond else ""))
    if not cond:
        failures.append(name)


def req(url, method="GET", body=None, headers=None):
    data = json.dumps(body).encode() if body is not None else None
    h = {"content-type": "application/json"} if body is not None else {}
    h.update(headers or {})
    r = urllib.request.Request(url, data=data, headers=h, method=method)
    try:
        resp = urllib.request.urlopen(r, timeout=8)
        return resp.status, {k.lower(): v for k, v in resp.headers.items()}, resp.read().decode()
    except urllib.error.HTTPError as e:
        return e.code, {k.lower(): v for k, v in e.headers.items()}, e.read().decode()


def wait_up(url, tries=60):
    for _ in range(tries):
        try:
            urllib.request.urlopen(url, timeout=2)
            return True
        except Exception:
            time.sleep(0.3)
    return False


def main():
    tmp = tempfile.mkdtemp(prefix="coven-web-verify-")
    seed = os.path.join(tmp, "seed.hex")
    with open(seed, "w") as f:
        f.write("11223344556677889900aabbccddeeff11223344556677889900aabbccddeeff\n")
    os.makedirs(os.path.join(tmp, "registry"), exist_ok=True)

    # BUG-029: the real dist/ is gitignored and typically empty (the JS build is a separate,
    # node-dependent step). This harness verifies the SERVER's security + behavior contract, for
    # which the assets are just fixtures, so synthesize a minimal, writable served-assets dir
    # rather than depending on an ignored dir that may not exist. The webauthn/oauth handlers
    # write their `_wa_*`/`_oauth_*` state files here, so it must be writable.
    dist = os.path.join(tmp, "dist")
    os.makedirs(dist, exist_ok=True)
    with open(os.path.join(dist, "index.html"), "w") as f:
        f.write('<!doctype html><html lang="en"><head><meta charset="utf-8">'
                '<link rel="stylesheet" href="/styles.css"><title>Coven Web</title></head>'
                '<body><main id="app"></main><script src="/app.js"></script></body></html>')
    with open(os.path.join(dist, "app.js"), "w") as f:
        f.write("// coven-web app fixture\n")
    with open(os.path.join(dist, "styles.css"), "w") as f:
        f.write("/* coven-web styles fixture */\n")
    with open(os.path.join(dist, "source-sandbox.js"), "w") as f:
        f.write("// source-sandbox fixture\n")

    coven = subprocess.Popen(
        [BIN, "sandbox", "--dir", tmp, "--net", COVEN, "--signing-key", seed,
         os.path.join(REPO, "projects/coven/src/coven.witchy"), COVEN],
        cwd=REPO, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    # coven-web runs in the WASM sandbox (wasmtime VM memory isolation): the
    # fallible proxy dial (`try_connect`) and the WebAuthn ES256 verify
    # (`crypto.ecdsa_p256_verify_hex` + the sha512/sha3/hmac extensions) are now
    # bridged to host imports, so nothing is interpreter-only. `--dir` roots the
    # served-assets Dir at dist (the sandbox does not derive it from cwd).
    web = subprocess.Popen(
        [BIN, "sandbox", "--dir", dist,
         "--net", WEB, "--net", COVEN, "--signing-key", seed,
         os.path.join(REPO, "projects/coven-web/src/coven_web.witchy"), WEB, COVEN,
         "http://" + WEB, "localhost"],
        cwd=REPO, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        if not wait_up(COVEN_URL + "/coven/index"):
            check("coven starts", False, "no response"); return
        if not wait_up(WEB_URL + "/"):
            check("coven-web starts", False, "no response"); return

        # Seed a published rune (anonymous publish).
        manifest = '[rune]\nname = "demo/verify"\nversion = "1.0.0"\n\n[capabilities]\nruntime = []\n'
        src = 'pub fn id(x: Int) -> Int:\n    x\n'
        code, _, _ = req(COVEN_URL + "/coven/publish", "POST", {
            "manifest_toml": manifest, "uploaded_by": "ci",
            "source": {"files": [["witchy.toml", manifest], ["src/v.witchy", src]]}})
        check("publish a rune", code == 200, f"status {code}")

        # App shell: Perfect Types + strict cross-origin isolation.
        st, h, body = req(WEB_URL + "/")
        csp = h.get("content-security-policy", "")
        check("GET / 200", st == 200)
        check("app CSP enforces Perfect Types",
              "require-trusted-types-for 'script'" in csp and "trusted-types 'none'" in csp, csp)
        check("app CSP connect-src 'self'", "connect-src 'self'" in csp)
        check("COOP same-origin", h.get("cross-origin-opener-policy") == "same-origin")
        check("COEP require-corp", h.get("cross-origin-embedder-policy") == "require-corp")
        check("CORP same-origin", h.get("cross-origin-resource-policy") == "same-origin")
        check("nosniff", h.get("x-content-type-options") == "nosniff")
        check("X-Frame-Options DENY", h.get("x-frame-options") == "DENY")

        # Static MIME (critical under nosniff).
        _, h, _ = req(WEB_URL + "/app.js")
        check("app.js is text/javascript", "javascript" in h.get("content-type", ""))

        # Sandbox frame: its own CSP firewall.
        st, h, body = req(WEB_URL + "/sandbox-frame")
        scsp = h.get("content-security-policy", "")
        check("sandbox CSP connect-src 'none'", "connect-src 'none'" in scsp, scsp)
        check("sandbox CSP sandbox allow-scripts", "sandbox allow-scripts" in scsp)
        check("sandbox X-Frame-Options SAMEORIGIN", h.get("x-frame-options") == "SAMEORIGIN")
        check("sandbox bootstrap guards opaque origin", "window.origin" in body and "srcdoc" in body)

        # Strict isolation on /api too.
        _, h, _ = req(WEB_URL + "/api/coven/index")
        check("/api COOP/COEP/CORP strict",
              h.get("cross-origin-opener-policy") == "same-origin"
              and h.get("cross-origin-embedder-policy") == "require-corp"
              and h.get("cross-origin-resource-policy") == "same-origin")

        # Proxy returns coven data + source round-trip.
        _, _, body = req(WEB_URL + "/api/coven/index")
        check("proxy /index lists the rune", "demo/verify" in body, body[:120])
        _, _, body = req(WEB_URL + "/api/coven/source?name=demo~verify&version=1.0.0")
        check("proxy /source returns the source", "pub fn id" in body)

        # SECURITY (finding #6): the plain, session-only write routes are GONE. Promotion and
        # yank are reachable ONLY through the WebAuthn-2FA handlers (exercised below). A
        # same-origin POST — past the Sec-Fetch CSRF layer — to the OLD plain paths must now 404:
        # a bearer session (mintable by any social login) can no longer change registry state.
        so = {"sec-fetch-site": "same-origin"}
        st, _, _ = req(WEB_URL + "/api/coven/promote", "POST",
                       {"name": "demo/verify", "version": "1.0.0", "second_factor": "x", "promoted_by": "rel"}, so)
        check("plain session-only /api/coven/promote route removed (404)", st == 404, f"status {st}")
        st, _, _ = req(WEB_URL + "/api/coven/yank", "POST",
                       {"name": "demo/verify", "version": "1.0.0"}, so)
        check("plain session-only /api/coven/yank route removed (404)", st == 404, f"status {st}")

        # Unicode source survives the round-trip when published as raw UTF-8 (the
        # `\u`-escaped path is a separate known std/json decoder bug — see PLAN.md).
        uni = "// café 日本語 🌍\npub fn x() -> Int:\n    1\n"
        m2 = '[rune]\nname = "demo/uni"\nversion = "1.0.0"\n\n[capabilities]\nruntime = []\n'
        import json as _json
        d = _json.dumps({"manifest_toml": m2, "uploaded_by": "ci",
                         "source": {"files": [["witchy.toml", m2], ["src/u.witchy", uni]]}}, ensure_ascii=False).encode("utf-8")
        rq = urllib.request.Request(COVEN_URL + "/coven/publish", data=d, headers={"content-type": "application/json"}, method="POST")
        urllib.request.urlopen(rq, timeout=8)
        _, _, body = req(WEB_URL + "/api/coven/source?name=demo~uni&version=1.0.0")
        got = dict(_json.loads(body)["files"]).get("src/u.witchy", "")
        check("unicode source round-trips exactly (raw UTF-8)", got == uni)

        # Anti-SSRF: only allowlisted proxy paths route; others 404.
        st, _, _ = req(WEB_URL + "/api/coven/secrets")
        check("non-allowlisted /api path 404 (SSRF allowlist)", st == 404, f"status {st}")

        # Sec-Fetch CSRF layer — it fronts every state-changing route (here the 2FA yank) and
        # runs BEFORE the handler, so cross-site/same-site writes are refused independent of the
        # WebAuthn assertion. Same-origin admission is proven by the 2FA e2e below (promote/yank
        # both return 200 with the `so` same-origin header).
        yk = WEB_URL + "/api/coven/yank-2fa"
        bdy = {"name": "demo/verify", "version": "1.0.0"}
        st, _, _ = req(yk, "POST", bdy, {"sec-fetch-site": "cross-site"})
        check("CSRF: cross-site write 403", st == 403, f"status {st}")
        st, _, _ = req(yk, "POST", bdy, {"sec-fetch-site": "same-site"})
        check("CSRF: same-site write 403", st == 403, f"status {st}")

        # BUG-372: minting a challenge WRITES server state, so it is now a POST behind the CSRF
        # layer — never a state-changing GET (which would be CSRF-exempt). The old GET is gone,
        # and a cross-site POST is refused before the handler.
        st, _, _ = req(WEB_URL + "/api/webauthn/challenge")  # GET
        check("BUG-372: challenge rejects a state-changing GET (405/404)", st in (405, 404), f"status {st}")
        st, _, _ = req(WEB_URL + "/api/webauthn/challenge", "POST", {"op": "login"},
                       {"sec-fetch-site": "cross-site"})
        check("BUG-372: cross-site challenge POST refused by CSRF layer (403)", st == 403, f"status {st}")

        # WebAuthn 2FA: register a P-256 credential, sign IN for a bearer session, then drive
        # promote-2fa AND yank-2fa end-to-end. The server verifies each real ES256 assertion via
        # std/webauthn (challenge bound to the operation) before forwarding to coven.
        #
        # BUG-142: a missing `cryptography` is a HARD FAILURE, not a silent [SKIP]. The 2FA e2e is
        # a core part of the contract; an unrunnable prerequisite must fail the run, not pass it.
        try:
            from cryptography.hazmat.primitives.asymmetric import ec
            from cryptography.hazmat.primitives import hashes, serialization
            have_crypto = True
        except ImportError:
            have_crypto = False
        check("prerequisite: python `cryptography` is installed (2FA e2e)", have_crypto,
              "pip install cryptography")
        if have_crypto:
            import hashlib as _h, struct as _s, base64 as _b
            sk = ec.generate_private_key(ec.SECP256R1())
            pubpt = sk.public_key().public_bytes(
                serialization.Encoding.X962, serialization.PublicFormat.UncompressedPoint)
            wm = '[rune]\nname = "demo/wav"\nversion = "1.0.0"\n\n[capabilities]\nruntime = []\n'
            req(COVEN_URL + "/coven/publish", "POST", {"manifest_toml": wm, "uploaded_by": "ci",
                "source": {"files": [["witchy.toml", wm], ["src/x.witchy", "pub fn x() -> Int:\n    1\n"]]}})
            st, _, _ = req(WEB_URL + "/api/webauthn/register", "POST",
                           {"credentialId": "c1", "publicKey": pubpt.hex()}, so)
            check("webauthn: register credential", st == 200, f"status {st}")

            def challenge(op, name="", version=""):
                # BUG-365/372: the challenge POST body binds it (hence the assertion) to op+params.
                _, _, cb = req(WEB_URL + "/api/webauthn/challenge", "POST",
                               {"op": op, "name": name, "version": version}, so)
                return json.loads(cb)["challengeHex"]

            def wa_assert(chal_hex, origin, mut=lambda s: s):
                chal = bytes.fromhex(chal_hex)
                cd = '{"type":"webauthn.get","challenge":"%s","origin":"%s"}' % (
                    _b.urlsafe_b64encode(chal).rstrip(b"=").decode(), origin)
                ad = _h.sha256(b"localhost").digest() + bytes([0x05]) + _s.pack(">I", 1)
                sig = sk.sign(ad + _h.sha256(cd.encode()).digest(), ec.ECDSA(hashes.SHA256()))
                return ad.hex(), cd, mut(sig.hex())

            # Sign in with the passkey to mint a bearer session; promote binds `promoted_by` to it.
            ad, cd, sg = wa_assert(challenge("login"), "http://" + WEB)
            st, _, tb = req(WEB_URL + "/api/webauthn/login", "POST",
                {"credentialId": "c1", "authData": ad, "clientData": cd, "signature": sg}, so)
            check("webauthn: passkey sign-in mints a session", st == 200, f"status {st}")
            token = (json.loads(tb) if st == 200 else {}).get("token", "")
            check("webauthn: session token issued", token != "")
            auth = dict(so); auth["authorization"] = "Bearer " + token

            # BUG-278: a promote with NO session is refused — the promoter identity must be an
            # authenticated subject, never a client body field (fail closed).
            ad, cd, sg = wa_assert(challenge("promote", "demo/wav", "1.0.0"), "http://" + WEB)
            st, _, _ = req(WEB_URL + "/api/coven/promote-2fa", "POST",
                {"name": "demo/wav", "version": "1.0.0",
                 "credentialId": "c1", "authData": ad, "clientData": cd, "signature": sg}, so)
            check("BUG-278: promote without a session is refused (403)", st == 403, f"status {st}")

            # BUG-365: an assertion whose challenge was minted for a DIFFERENT rune is not accepted.
            ad, cd, sg = wa_assert(challenge("promote", "demo/other", "1.0.0"), "http://" + WEB)
            st, _, _ = req(WEB_URL + "/api/coven/promote-2fa", "POST",
                {"name": "demo/wav", "version": "1.0.0",
                 "credentialId": "c1", "authData": ad, "clientData": cd, "signature": sg}, auth)
            check("BUG-365: challenge bound to a different rune is refused (403)", st == 403, f"status {st}")

            # Valid, session-authenticated, operation-bound promote: staged -> released. The
            # recorded promoter is the session subject ("passkey"), which differs from the
            # publisher ("ci"), satisfying coven's separation of duties.
            ad, cd, sg = wa_assert(challenge("promote", "demo/wav", "1.0.0"), "http://" + WEB)
            st, _, body = req(WEB_URL + "/api/coven/promote-2fa", "POST",
                {"name": "demo/wav", "version": "1.0.0",
                 "credentialId": "c1", "authData": ad, "clientData": cd, "signature": sg}, auth)
            check("webauthn 2FA: valid assertion promotes (staged->released)",
                  st == 200 and '"state":"released"' in body, f"status {st} body {body[:160]}")

            # BUG-421: that promote CONSUMED the challenge (cleared to `{}`). Replaying the SAME
            # assertion without minting a fresh challenge is refused — single-use enforced by
            # content, not mere file existence.
            st, _, _ = req(WEB_URL + "/api/coven/promote-2fa", "POST",
                {"name": "demo/wav", "version": "1.0.0",
                 "credentialId": "c1", "authData": ad, "clientData": cd, "signature": sg}, auth)
            check("BUG-421: replayed (consumed) challenge is refused (403)", st == 403, f"status {st}")

            # Tampered signature is rejected even with a fresh, correctly-bound challenge.
            ad, cd, sg = wa_assert(challenge("promote", "demo/wav", "1.0.0"), "http://" + WEB, lambda s: "00" + s)
            st, _, _ = req(WEB_URL + "/api/coven/promote-2fa", "POST",
                {"name": "demo/wav", "version": "1.0.0",
                 "credentialId": "c1", "authData": ad, "clientData": cd, "signature": sg}, auth)
            check("webauthn 2FA: tampered signature rejected (403)", st == 403, f"status {st}")

            # WebAuthn 2FA yank: a fresh valid assertion yanks the just-released version; a tampered
            # one is refused — proving yank, too, is WebAuthn-gated (not session-only).
            ad, cd, sg = wa_assert(challenge("yank", "demo/wav", "1.0.0"), "http://" + WEB)
            st, _, body = req(WEB_URL + "/api/coven/yank-2fa", "POST",
                {"name": "demo/wav", "version": "1.0.0",
                 "credentialId": "c1", "authData": ad, "clientData": cd, "signature": sg}, auth)
            check("webauthn 2FA: valid assertion yanks (released->yanked)",
                  st == 200 and '"ok":true' in body, f"status {st} body {body[:120]}")
            ad, cd, sg = wa_assert(challenge("yank", "demo/wav", "1.0.0"), "http://" + WEB, lambda s: "00" + s)
            st, _, _ = req(WEB_URL + "/api/coven/yank-2fa", "POST",
                {"name": "demo/wav", "version": "1.0.0",
                 "credentialId": "c1", "authData": ad, "clientData": cd, "signature": sg}, auth)
            check("webauthn 2FA: tampered yank signature rejected (403)", st == 403, f"status {st}")

        # B6 (proxy resilience): a down upstream yields 502, not a crash. Kill coven
        # (must be the last check — it leaves the upstream down), then probe.
        coven.send_signal(signal.SIGTERM)
        time.sleep(0.6)
        st, _, _ = req(WEB_URL + "/api/coven/index")
        check("B6: proxy returns 502 when coven is down (no crash)", st == 502, f"status {st}")
        st, _, _ = req(WEB_URL + "/")
        check("B6: server survives a down upstream", st == 200, f"status {st}")
    finally:
        for p in (web, coven):
            try:
                p.send_signal(signal.SIGTERM)
            except Exception:
                pass

    print()
    if failures:
        print(f"FAILED ({len(failures)}): " + ", ".join(failures))
        sys.exit(1)
    print("ALL CHECKS PASSED")


if __name__ == "__main__":
    main()
