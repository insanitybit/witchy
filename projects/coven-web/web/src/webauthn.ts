// Browser-side WebAuthn ceremony. The credential API yields binary ArrayBuffers;
// we hex-encode them at the boundary so coven-web verifies with text/hex ops. The
// public key comes from getPublicKey() (SPKI DER) → SEC1 uncompressed point — no CBOR.
import { authHeader } from "./session";

// A client-side deadline over the WebAuthn ceremony. `navigator.credentials.create/get`
// carry an authenticator `timeout`, but a stalled platform dialog (or an authenticator that
// never resolves) can leave that promise pending forever, sticking the UI on "registering…".
// Race the ceremony against a hard wall-clock cap so the caller ALWAYS settles (UI-04).
function withTimeout<T>(p: Promise<T>, ms: number, label: string): Promise<T> {
  let timer: ReturnType<typeof setTimeout>;
  const guard = new Promise<never>((_, reject) => {
    timer = setTimeout(() => reject(new Error(label + " timed out")), ms);
  });
  return Promise.race([p, guard]).finally(() => clearTimeout(timer)) as Promise<T>;
}

// Hard client-side deadline for a credential ceremony — a hair past the authenticator's own
// 60s `timeout`, so the native cancel normally fires first and this only catches true stalls.
const CEREMONY_TIMEOUT_MS = 65000;

function hex(buf: ArrayBuffer): string {
  return Array.from(new Uint8Array(buf))
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");
}

function hexToBytes(h: string): Uint8Array<ArrayBuffer> {
  const out = new Uint8Array(h.length / 2);
  for (let i = 0; i < out.length; i++) out[i] = parseInt(h.slice(i * 2, i * 2 + 2), 16);
  return out;
}

function b64url(buf: ArrayBuffer): string {
  let s = "";
  const b = new Uint8Array(buf);
  for (let i = 0; i < b.length; i++) s += String.fromCharCode(b[i]);
  return btoa(s).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

// SPKI DER (from getPublicKey()) → SEC1 uncompressed point hex (the last 65 bytes
// for P-256: 0x04 ‖ x ‖ y). This is exactly what std/webauthn expects as the pubkey.
function spkiToSec1Hex(spki: ArrayBuffer): string {
  const b = new Uint8Array(spki);
  return hex(b.slice(b.length - 65).buffer);
}

// Mint a fresh single-use challenge for a specific operation. It is a POST (state-changing:
// it writes the server's outstanding challenge, so it must sit behind the Sec-Fetch CSRF
// layer, never GET). The {op, name, version} body BINDS the challenge — and thus the assertion
// the authenticator signs over it — to that exact operation, so the server rejects an assertion
// minted for anything else (coven_web.witchy h_wa_challenge / wa_do_promote / wa_do_yank).
async function challengeBytes(op: string, name = "", version = ""): Promise<Uint8Array<ArrayBuffer>> {
  const r = await fetch("/api/webauthn/challenge", {
    method: "POST",
    credentials: "omit",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ op, name, version }),
  });
  if (!r.ok) throw new Error("challenge request failed (" + r.status + ")");
  const { challengeHex } = (await r.json()) as { challengeHex: string };
  return hexToBytes(challengeHex);
}

// Register a passkey (P-256/ES256) and bind its public key on the server.
export async function register(rpId: string): Promise<void> {
  const cred = (await withTimeout(
    navigator.credentials.create({
      publicKey: {
        challenge: await challengeBytes("register"),
        rp: { id: rpId, name: "coven" },
        user: { id: new Uint8Array([1]), name: "maintainer", displayName: "maintainer" },
        // Offer ES256 first (the P-256 key this shell decodes via spkiToSec1Hex), then RS256 as a
        // fallback for authenticators that reject an ES256-only list — Chrome warns an ES256-only
        // offer can fail on incompatible authenticators (UI-05). ES256 stays the preferred choice.
        pubKeyCredParams: [
          { type: "public-key", alg: -7 },
          { type: "public-key", alg: -257 },
        ],
        // A discoverable (resident) passkey, so the assertion can find it without the
        // server having to hand back an allow-list of credential ids.
        authenticatorSelection: { residentKey: "required", requireResidentKey: true, userVerification: "required" },
        timeout: 60000,
      },
    }),
    CEREMONY_TIMEOUT_MS,
    "passkey registration",
  )) as PublicKeyCredential;
  const resp = cred.response as AuthenticatorAttestationResponse;
  const spki = resp.getPublicKey();
  if (!spki) throw new Error("authenticator returned no public key");
  // BUG-018: check r.ok. Registration can be REFUSED server-side (e.g. a passkey is already
  // registered → 403). Ignoring the response let a rejected register silently look like success;
  // the caller would then flip the UI to a "registered" state that the server never granted.
  const r = await fetch("/api/webauthn/register", {
    method: "POST",
    credentials: "omit",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ credentialId: b64url(cred.rawId), publicKey: spkiToSec1Hex(spki) }),
  });
  if (!r.ok) {
    const d = (await r.json().catch(() => ({}))) as { error?: string };
    throw new Error(d.error ?? "registration failed (" + r.status + ")");
  }
}

// Sign in: run the assertion ceremony and exchange it for a session token. The
// server verifies the assertion against the registered credential + single-use
// challenge (h_wa_login) and, only then, mints the signed bearer token.
export async function login(rpId: string): Promise<string> {
  const assertion = (await navigator.credentials.get({
    publicKey: { challenge: await challengeBytes("login"), rpId, userVerification: "required", timeout: 60000 },
  })) as PublicKeyCredential;
  const resp = assertion.response as AuthenticatorAssertionResponse;
  const r = await fetch("/api/webauthn/login", {
    method: "POST",
    credentials: "omit",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      credentialId: b64url(assertion.rawId),
      authData: hex(resp.authenticatorData),
      clientData: new TextDecoder().decode(resp.clientDataJSON),
      signature: hex(resp.signature),
    }),
  });
  const data = (await r.json().catch(() => ({}))) as { token?: string; error?: string };
  if (!r.ok || !data.token) throw new Error(data.error ?? "sign-in failed (" + r.status + ")");
  return data.token;
}

// Run the assertion ceremony and POST the (hex-encoded) assertion to a 2FA write route.
// Both state-changing writes (promote, yank) go through a fresh, single-use WebAuthn
// assertion the server verifies before forwarding to coven — a session bearer alone is
// never sufficient authority. `extra` carries the route-specific body fields.
async function wa_write(
  route: string,
  rpId: string,
  op: string,
  name: string,
  version: string,
): Promise<Response> {
  const assertion = (await navigator.credentials.get({
    publicKey: {
      // Bind the challenge (hence the assertion) to THIS operation + name@version.
      challenge: await challengeBytes(op, name, version),
      rpId,
      userVerification: "required",
      timeout: 60000,
    },
  })) as PublicKeyCredential;
  const resp = assertion.response as AuthenticatorAssertionResponse;
  return fetch(route, {
    method: "POST",
    credentials: "omit",
    // Attach the bearer session: the server binds `promoted_by` to the authenticated subject
    // (never a client-supplied field), so a write must carry a valid session (BUG-278).
    headers: { "content-type": "application/json", ...authHeader() },
    body: JSON.stringify({
      name,
      version,
      credentialId: b64url(assertion.rawId),
      authData: hex(resp.authenticatorData),
      clientData: new TextDecoder().decode(resp.clientDataJSON),
      signature: hex(resp.signature),
    }),
  });
}

// Promote a staged version to released — verified by a WebAuthn 2FA assertion server-side. The
// promoter identity is derived from the bearer session server-side, NOT sent from here.
export function promote2fa(rpId: string, name: string, version: string): Promise<Response> {
  return wa_write("/api/coven/promote-2fa", rpId, "promote", name, version);
}

// Yank a version — like promote, a destructive state change gated by a WebAuthn 2FA assertion.
export function yank2fa(rpId: string, name: string, version: string): Promise<Response> {
  return wa_write("/api/coven/yank-2fa", rpId, "yank", name, version);
}
