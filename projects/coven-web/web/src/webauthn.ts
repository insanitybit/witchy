// Browser-side WebAuthn ceremony. The credential API yields binary ArrayBuffers;
// we hex-encode them at the boundary so coven-web verifies with text/hex ops. The
// public key comes from getPublicKey() (SPKI DER) → SEC1 uncompressed point — no CBOR.

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

async function challengeBytes(): Promise<Uint8Array<ArrayBuffer>> {
  const r = await fetch("/api/webauthn/challenge", { credentials: "omit" });
  const { challengeHex } = (await r.json()) as { challengeHex: string };
  return hexToBytes(challengeHex);
}

// Register a passkey (P-256/ES256) and bind its public key on the server.
export async function register(rpId: string): Promise<void> {
  const cred = (await navigator.credentials.create({
    publicKey: {
      challenge: await challengeBytes(),
      rp: { id: rpId, name: "coven" },
      user: { id: new Uint8Array([1]), name: "maintainer", displayName: "maintainer" },
      pubKeyCredParams: [{ type: "public-key", alg: -7 }],
      // A discoverable (resident) passkey, so the assertion can find it without the
      // server having to hand back an allow-list of credential ids.
      authenticatorSelection: { residentKey: "required", requireResidentKey: true, userVerification: "required" },
      timeout: 60000,
    },
  })) as PublicKeyCredential;
  const resp = cred.response as AuthenticatorAttestationResponse;
  const spki = resp.getPublicKey();
  if (!spki) throw new Error("authenticator returned no public key");
  await fetch("/api/webauthn/register", {
    method: "POST",
    credentials: "omit",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ credentialId: b64url(cred.rawId), publicKey: spkiToSec1Hex(spki) }),
  });
}

// Run the assertion ceremony and POST the (hex-encoded) assertion to the 2FA promote.
export async function promote2fa(
  rpId: string,
  name: string,
  version: string,
  promotedBy: string,
): Promise<Response> {
  const assertion = (await navigator.credentials.get({
    publicKey: {
      challenge: await challengeBytes(),
      rpId,
      userVerification: "required",
      timeout: 60000,
    },
  })) as PublicKeyCredential;
  const resp = assertion.response as AuthenticatorAssertionResponse;
  return fetch("/api/coven/promote-2fa", {
    method: "POST",
    credentials: "omit",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      name,
      version,
      promotedBy,
      credentialId: b64url(assertion.rawId),
      authData: hex(resp.authenticatorData),
      clientData: new TextDecoder().decode(resp.clientDataJSON),
      signature: hex(resp.signature),
    }),
  });
}
