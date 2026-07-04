// coven-web bootstrap — the THIN, capability-holding JS host shell for the coven-web app,
// which is now ONE capability-pure glamour rune (projects/glamour/examples/coven_web_app),
// compiled to WASM and base64-inlined below at build time (RFC-0015 Phase D). This file is
// the entire hand-written frontend: it holds ALL authority — the network, the bearer session
// token, and the WebAuthn ceremony — and hands the rune only data + narrow port results, so
// the rune never sees a credential and the app stays XSS/CSRF-proof by construction. The
// rune renders via glamour-dom.mjs (createElement/textContent only — no HTML-string sink).
import { mount } from "glamour-dom";
import { authHeader, setToken, isSignedIn } from "./session";
import { login as passkeyLogin, register as passkeyRegister, promote2fa, yank2fa } from "./webauthn";

// The compiled glamour app, base64. build.sh replaces this placeholder with the bytes of
// `coven_web_app.wasm` (witchy's Dir `read` is UTF-8-only and cannot serve binary, so — as
// with source-sandbox.js's highlighter — the module rides inlined in the bundle the server
// already serves as text under `script-src 'self'`; the parent compiles it under the app
// CSP's `'wasm-unsafe-eval'`, the only authority the trusted shell needs beyond the network).
const APP_WASM_B64 = "__APP_WASM_B64__";

function wasmBytes(): Uint8Array<ArrayBuffer> {
  const bin = atob(APP_WASM_B64);
  const u8 = new Uint8Array(new ArrayBuffer(bin.length));
  for (let i = 0; i < bin.length; i++) u8[i] = bin.charCodeAt(i);
  return u8;
}

// "/v/<wire-name>/<version>" (or "/d/...") -> [display-name, version]. The rune wire-encodes
// `/` as `~` in route paths; the write ports talk to coven in display form, so undo it here.
function nameVer(route: string): [string, string] {
  const rest = route.slice(3);
  const slash = rest.indexOf("/");
  const wire = slash < 0 ? rest : rest.slice(0, slash);
  const version = slash < 0 ? "" : rest.slice(slash + 1);
  return [wire.replace(/~/g, "/"), version];
}

// A social-login callback lands on `/#token=<sess>&login=<who>` (coven_web.witchy
// session_redirect). Adopt the token (so authHeader() carries it) and surface the identity,
// then scrub the fragment so a reload/Referer never leaks it.
function adoptLoginFragment(): string {
  const raw = location.hash.startsWith("#") ? location.hash.slice(1) : location.hash;
  const params = new URLSearchParams(raw);
  const token = params.get("token");
  const who = params.get("login") ?? "";
  if (token) {
    setToken(token);
    history.replaceState({}, "", location.pathname);
    return who;
  }
  return "";
}

async function boot(): Promise<void> {
  const root = document.getElementById("app");
  if (!root) throw new Error("coven-web: missing #app element");

  const who = adoptLoginFragment();
  const session = who || (isSignedIn() ? "maintainer" : "");

  await mount(wasmBytes(), root, {
    initialModel: { route: location.pathname, session, data: "", notice: "" },
    routeTag: "Route",
    // The shell owns the network: fetch same-origin, NEVER with ambient cookies (so a
    // cross-site request carries no credential — CSRF has nothing to ride), and attach the
    // bearer session ITSELF via authHeaders so the rune never holds the token.
    fetch: (url: string, init?: RequestInit) => fetch(url, { ...init, credentials: "omit" }),
    authHeaders: authHeader,
    // Host capabilities the rune cannot perform — the WebAuthn ceremony and the gated writes.
    // Each runs HERE (the credential/navigator.credentials/token stay in the host) and returns
    // only a short display string the rune folds into its status notice.
    ports: {
      passkeyLogin: async () => {
        const tok = await passkeyLogin(location.hostname);
        setToken(tok);
        return "maintainer";
      },
      passkeyRegister: async () => {
        await passkeyRegister(location.hostname);
        return "passkey registered — now sign in";
      },
      promote: async (route: string) => {
        const [name, version] = nameVer(route);
        const r = await promote2fa(location.hostname, name, version, "maintainer@demo");
        if (r.ok) return "released ✓";
        const d = (await r.json().catch(() => ({}))) as { error?: string };
        return "refused: " + (d.error ?? r.status);
      },
      yank: async (route: string) => {
        const [name, version] = nameVer(route);
        // Yank is a destructive state change, so — like promote — it runs the WebAuthn
        // 2FA ceremony HERE (credential/token stay in the host) and the server re-verifies
        // the assertion before forwarding to coven. A session bearer alone can't yank.
        const r = await yank2fa(location.hostname, name, version);
        if (r.ok) return "yanked ✓";
        const d = (await r.json().catch(() => ({}))) as { error?: string };
        return "refused: " + (d.error ?? r.status);
      },
    },
  });
}

void boot();
