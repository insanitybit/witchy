// Client-side session state: the bearer token minted by the server after a passkey
// sign-in (see webauthn.login + coven_web.witchy h_wa_login). Writes attach it via
// authHeader(); the server re-verifies its signature on every state-changing call,
// so this token is only a convenience cache — losing it just means signing in again.
const KEY = "coven-session";

let token: string | null = readStored();
const subscribers = new Set<() => void>();

function readStored(): string | null {
  try {
    return sessionStorage.getItem(KEY);
  } catch {
    return null; // storage can be unavailable (private mode); sessions are then memory-only
  }
}

export function isSignedIn(): boolean {
  return token !== null;
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
  subscribers.forEach((f) => f());
}

export function subscribe(f: () => void): void {
  subscribers.add(f);
}
