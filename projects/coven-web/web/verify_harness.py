#!/usr/bin/env python3
"""Static verification harness for the WS-I/M6 glamour-WASM source sandbox.

The real coven-web witchy server cannot boot on the current binary (its stdlib
lacks `http.try_get`; this harness therefore serves
`web/dist/` with coven-web's EXACT per-route CSP + header stack (the strings are
read verbatim from src/coven_web.witchy so they cannot drift), plus the identical
`/sandbox-frame` bootstrap, and a `/test` page that runs the trusted-parent
sandbox flow (a faithful copy of web/src/sandbox.ts) against an untrusted source
snippet. Playwright then asserts the security invariants.
"""
import http.server
import os
import re
import socketserver
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
DIST = os.path.join(HERE, "dist")
WITCHY = os.path.join(HERE, "..", "src", "coven_web.witchy")
PORT = int(sys.argv[1]) if len(sys.argv) > 1 else 18090

# Pull the exact CSP strings + the sandbox bootstrap out of the witchy server so
# this harness mirrors production headers byte-for-byte (no hand-copied drift).
_src = open(WITCHY).read()


def _string_after(marker):
    # Grab the first "..."-quoted literal on the line containing `marker`'s fn body.
    idx = _src.index(marker)
    rest = _src[idx:]
    m = re.search(r'"((?:[^"\\]|\\.)*)"', rest)
    return m.group(1)


APP_CSP = _string_after("fn app_csp() -> String:")
SANDBOX_CSP = _string_after("fn sandbox_csp() -> String:")
# The bootstrap HTML literal (sandbox_bootstrap()).
BOOTSTRAP = _string_after("fn sandbox_bootstrap() -> String:")

COMMON = {
    "x-content-type-options": "nosniff",
    "referrer-policy": "no-referrer",
    "cross-origin-opener-policy": "same-origin",
    "cross-origin-embedder-policy": "require-corp",
    "cross-origin-resource-policy": "same-origin",
    "document-isolation-policy": "isolate-and-require-corp",
    "cache-control": "no-store",
}

# An untrusted witchy snippet whose string literal embeds a live <script> — the
# injection probe. It must render as INERT text, never a real <script> element.
TEST_SOURCE = (
    "fn main(console: Console):\n"
    "    // greet, then try to inject\n"
    '    let msg = "<script>alert(1)</script>"\n'
    "    print(console, msg)\n"
)

# The test page loads its driver as an EXTERNAL script (`/test.js`) — exactly how
# the real parent loads `app.js` — so the strict app CSP `script-src 'self'`
# permits it (an inline script would be correctly blocked by Perfect Types).
TEST_PAGE = """<!doctype html><html><head><meta charset='utf-8'><title>sandbox test</title></head>
<body><div id='host'></div><div id='status'>booting</div>
<script src='/test.js'></script></body></html>"""

# A faithful copy of web/src/sandbox.ts's mountSandbox flow (the trusted parent):
# fetch source-sandbox.js as TEXT, build an opaque-origin outer iframe pointed at
# /sandbox-frame, hand it the inner srcdoc + a MessageChannel port, and send ONLY
# the source text. Results land on window.__probe for Playwright.
TEST_JS = """
const SOURCE = %s;
async function fetchText(url) {
  // Retry around transient ERR_NETWORK_IO_SUSPENDED (the headless tab can be
  // suspended between automation calls); a real browser tab does not see this.
  for (let i = 0; i < 30; i++) {
    try {
      const r = await fetch(url, { credentials: 'omit' });
      if (r.ok) return r.text();
    } catch (e) { /* retry */ }
    await new Promise((res) => setTimeout(res, 200));
  }
  throw new Error('fetchText gave up: ' + url);
}
async function main() {
  const sandboxJs = await fetchText('/source-sandbox.js');
  const innerHtml =
    "<!doctype html><html><head><meta charset='utf-8'></head><body>" +
    "<div id='out'></div><script>" + sandboxJs + "<" + "/script></body></html>";
  const outer = document.createElement('iframe');
  outer.setAttribute('sandbox', 'allow-scripts'); // no allow-same-origin => opaque origin
  outer.src = '/sandbox-frame';
  outer.addEventListener('load', () => {
    const win = outer.contentWindow;
    const ch = new MessageChannel();
    ch.port1.onmessage = (e) => {
      const m = e.data;
      if (m.type === 'ready') {
        ch.port1.postMessage({ type: 'render', kind: 'witchy-source', text: SOURCE });
      } else if (m.type === 'height') {
        outer.style.height = ((m.px||0) + 24) + 'px';
        window.__probe = window.__probe || {};
        window.__probe.height = m.px;
        window.__probe.networkBlocked = m.networkBlocked;
        window.__probe.dom = m.dom;
        window.__probe.done = true;
        document.getElementById('status').textContent =
          'rendered; networkBlocked=' + m.networkBlocked;
      }
    };
    win.postMessage({ type: 'init', html: innerHtml }, '*', [ch.port2]);
  });
  document.getElementById('host').appendChild(outer);
  window.__sandboxFrame = outer;
}
// Expose a manual trigger so the automation can start the flow while the tab is
// active; also auto-run on load for a normal browser.
window.runSandbox = () => { main().catch((e) => {
  document.getElementById('status').textContent = 'driver error: ' + e.message;
  window.__probe = { done: true, error: e.message };
}); };
window.runSandbox();
"""


class H(http.server.BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def _send(self, body, ctype, csp):
        if isinstance(body, str):
            body = body.encode("utf-8")
        self.send_response(200)
        self.send_header("content-type", ctype)
        self.send_header("content-length", str(len(body)))
        self.send_header("content-security-policy", csp)
        xfo = "SAMEORIGIN" if self.path == "/sandbox-frame" else "DENY"
        self.send_header("x-frame-options", xfo)
        for k, v in COMMON.items():
            self.send_header(k, v)
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        p = self.path.split("?", 1)[0]
        if p == "/test":
            return self._send(TEST_PAGE, "text/html; charset=utf-8", APP_CSP)
        if p == "/test.js":
            import json as _j
            js = TEST_JS % _j.dumps(TEST_SOURCE)
            return self._send(js, "text/javascript; charset=utf-8", APP_CSP)
        if p == "/sandbox-frame":
            return self._send(BOOTSTRAP, "text/html; charset=utf-8", SANDBOX_CSP)
        if p == "/source-sandbox.js":
            with open(os.path.join(DIST, "source-sandbox.js"), "rb") as f:
                return self._send(f.read(), "text/javascript; charset=utf-8", APP_CSP)
        self.send_response(404)
        self.end_headers()


class TCP(socketserver.TCPServer):
    allow_reuse_address = True


print(f"harness on http://127.0.0.1:{PORT}/test  (APP_CSP has wasm-unsafe-eval? "
      f"{'wasm-unsafe-eval' in APP_CSP}; SANDBOX_CSP has it? {'wasm-unsafe-eval' in SANDBOX_CSP})",
      flush=True)
TCP(("127.0.0.1", PORT), H).serve_forever()
