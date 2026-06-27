// The one write the glamour shell delegates to a host port: yank. (All READS are performed by
// the rune itself via the http effect — the shell only attaches the session header; see
// glamour-dom.mjs. promote runs the WebAuthn 2FA ceremony in webauthn.ts.) coven does NO
// url-decoding, so callers pass names in display form and coven matches them verbatim.
import { authHeader } from "./session";

async function postJSON<T>(path: string, body: unknown): Promise<T> {
  const r = await fetch(path, {
    method: "POST",
    credentials: "omit",
    // Carry the passkey session token; the server gates yank on it (require_session).
    headers: { "content-type": "application/json", ...authHeader() },
    body: JSON.stringify(body),
  });
  const data = (await r.json()) as T & { error?: string };
  if (!r.ok) throw new Error(data.error ?? "HTTP " + r.status);
  return data;
}

export function yank(name: string, version: string): Promise<{ ok: boolean }> {
  return postJSON<{ ok: boolean }>("/api/coven/yank", { name, version });
}
