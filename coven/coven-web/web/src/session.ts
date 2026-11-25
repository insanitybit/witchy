// Client-side session state: the bearer token minted by the server after a passkey
// sign-in (see webauthn.login + coven_web.witchy h_wa_login). Writes attach it via
// authHeader(); the server re-verifies its signature on every state-changing call,
// so this token is only a convenience cache — losing it just means signing in again.
const KEY = "coven-session";
// The human-readable identity the sign-in established (a social-login handle, or the
// "passkey" placeholder for an anonymous bearer). Persisted ALONGSIDE the token so a
// page reload keeps labelling the session honestly — otherwise a GitHub sign-in, whose
// handle only ever arrives in the one-shot `/#login=…` callback fragment, would decay to
// the generic passkey label on the next load (the bug that made a GitHub session read as
// "signed in as signed in (passkey)").
const ID_KEY = "coven-session-id";

let token: string | null = readStored(KEY);
let identity: string | null = readStored(ID_KEY);
const subscribers = new Set<() => void>();

function readStored(key: string): string | null {
  try {
    return sessionStorage.getItem(key);
  } catch {
    return null; // storage can be unavailable (private mode); sessions are then memory-only
  }
}

export function isSignedIn(): boolean {
  return token !== null;
}

// The persisted session identity, or null if none survives (fresh load, no sign-in).
export function sessionIdentity(): string | null {
  return identity;
}

// Record who the current session is. Persisted so it survives reloads; cleared on sign-out
// (setToken(null)).
export function setIdentity(who: string | null): void {
  identity = who;
  try {
    if (who) sessionStorage.setItem(ID_KEY, who);
    else sessionStorage.removeItem(ID_KEY);
  } catch {
    // storage unavailable — keep the in-memory identity only
  }
}

export function authHeader(): Record<string, string> {
  return token ? { authorization: "Bearer " + token } : {};
}

export function setToken(next: string | null): void {
  token = next;
  try {
    if (next) sessionStorage.setItem(KEY, next);
    else sessionStorage.removeItem(KEY);
  } catch {
    // storage unavailable — keep the in-memory token only
  }
  // Sign-out clears the persisted identity too; a bearer and its label live and die together.
  if (!next) setIdentity(null);
  subscribers.forEach((f) => f());
}

export function subscribe(f: () => void): void {
  subscribers.add(f);
}
