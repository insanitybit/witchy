// coven-web bootstrap — the THIN, capability-holding JS host shell for the coven-web app,
// which is now ONE capability-pure glamour rune (projects/glamour/examples/coven_web_app),
// compiled to WASM and base64-inlined below at build time (RFC-0015 Phase D). This file is
// the entire hand-written frontend: it holds ALL authority — the network, the bearer session
// token, and the WebAuthn ceremony — and hands the rune only data + narrow port results, so
// the rune never sees a credential and the app stays XSS/CSRF-proof by construction. The
// rune renders via glamour-dom.mjs (createElement/textContent only — no HTML-string sink).
import { mount } from "glamour-dom";
import { authHeader, setToken, isSignedIn, setIdentity, sessionIdentity } from "./session";
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
    // Persist the social-login handle so a later reload — after this one-shot fragment is
    // scrubbed — still labels the session with the real identity instead of decaying to the
    // generic passkey placeholder.
    if (who) setIdentity(who);
    history.replaceState({}, "", location.pathname);
    return who;
  }
  return "";
}

// Which social-login providers this instance has configured, read from the server-injected
// `<meta name="coven-login-providers" content="github google">` (coven_web.witchy builds the
// content from whether the GitHub/Google client ids are set). The rune renders a "sign in
// with <provider>" link only for the providers listed here, so a passkey-only deployment
// never shows a button whose `/auth/<provider>/login` route would 404. Unknown tokens are
// dropped — the rune only knows github/google.
function loginProviders(): string[] {
  const meta = document.querySelector('meta[name="coven-login-providers"]');
  const content = meta?.getAttribute("content") ?? "";
  return content.split(/\s+/).filter((p) => p === "github" || p === "google");
}

// Turn a promote/yank rejection into an honest, actionable status line. A *trusted* registry
// (the hosted default) requires a short-lived identity token carrying a human second factor to
// release or yank — a browser passkey assertion is not that token, so coven refuses. Rather than
// echo the raw server string, say plainly that this action isn't a browser operation on a trusted
// registry and where it does belong (a CI job's OIDC identity). Non-trust refusals pass through.
function refusalNotice(op: "promote" | "yank", error: string | undefined, status: number): string {
  if (error && /short-lived identity token/i.test(error)) {
    return `${op} needs a short-lived identity token — this registry uses trusted publishing, so ${op} runs from CI (\`witchy pm ${op}\`), not the browser.`;
  }
  return "refused: " + (error ?? status);
}

async function boot(): Promise<void> {
  const root = document.getElementById("app");
  if (!root) throw new Error("coven-web: missing #app element");

  const who = adoptLoginFragment();
  // Label the session HONESTLY (UI-03), and identify it correctly ACROSS RELOADS. The rune
  // renders this as "signed in as ${session}", so the string is a bare identity — never one
  // that already contains "signed in" (that produced the doubled "signed in as signed in
  // (passkey)"). Precedence: the fresh social-login handle from this callback, else the
  // identity persisted from an earlier sign-in (a GitHub handle survives the reload now), else
  // the honest "passkey" placeholder when all we know is that an anonymous bearer is present.
  // We never claim a named "maintainer" role the user was not granted.
  const session = who || sessionIdentity() || (isSignedIn() ? "passkey" : "");

  await mount(wasmBytes(), root, {
    initialModel: { route: location.pathname, session, data: "", notice: "", providers: loginProviders() },
    routeTag: "Route",
    // (RFC-0040/0107) `export_step` takes a `UiRoot`; stage its user-capability grant at
    // instantiation. Without this the compiled rune's `build_user_cap_field` traps at boot
    // (`user_cap_field_len — no [user_caps] grant`) and the app mounts blank. The
    // glamour-coven-web-app.test.mjs reference has always mounted with this grant; the
    // production host shell had drifted from it (BUG-608).
    instantiateOpts: { userCaps: [["coven-web"]] },
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
        // The bearer is anonymous — report the honest bare "passkey" identity (the rune renders
        // it as "signed in as passkey"), not a "maintainer" role the user does not hold (UI-03).
        // Persist it so a reload keeps the same honest label.
        setIdentity("passkey");
        return "passkey";
      },
      passkeyRegister: async () => {
        // UI-04: the ceremony can stall or be refused. webauthn.register now enforces a hard
        // client-side timeout; catch any failure here and return a short human notice (mirroring
        // the promote/yank "refused: …" shape) so the rune's status never sticks on "registering…".
        try {
          await passkeyRegister(location.hostname);
          return "passkey registered — now sign in";
        } catch (e) {
          return "register failed: " + (e instanceof Error ? e.message : String(e));
        }
      },
      promote: async (route: string) => {
        const [name, version] = nameVer(route);
        // The promoter identity is bound server-side to the authenticated session (BUG-278);
        // the host attaches the bearer via authHeaders, so we send only name/version here.
        const r = await promote2fa(location.hostname, name, version);
        if (r.ok) return "released ✓";
        const d = (await r.json().catch(() => ({}))) as { error?: string };
        return refusalNotice("promote", d.error, r.status);
      },
      yank: async (route: string) => {
        const [name, version] = nameVer(route);
        // Yank is a destructive state change, so — like promote — it runs the WebAuthn
        // 2FA ceremony HERE (credential/token stay in the host) and the server re-verifies
        // the assertion before forwarding to coven. A session bearer alone can't yank.
        const r = await yank2fa(location.hostname, name, version);
        if (r.ok) return "yanked ✓";
        const d = (await r.json().catch(() => ({}))) as { error?: string };
        return refusalNotice("yank", d.error, r.status);
      },
    },
  });
}

void boot();
