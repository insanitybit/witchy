// coven-web client entry. Zero runtime dependencies. The trusted parent builds
// DOM with createElement/textContent only (see dom.ts) and never inserts HTML
// strings, so Perfect Types (`trusted-types 'none'`) has nothing to catch here.
import { renderIndex } from "./views/index";
import { renderRune } from "./views/rune";
import { renderVersion } from "./views/version";
import { renderTrust } from "./views/trust";
import { clear, el, button } from "./dom";
import { isSignedIn, setToken, subscribe } from "./session";
import { register as registerPasskey, login as passkeyLogin } from "./webauthn";
import type { Nav } from "./nav";

const app = document.getElementById("app");
if (!app) throw new Error("coven-web: missing #app element");

// Track the active view so a sign-in/out can re-render it (write controls appear
// or vanish without a manual reload).
let current: () => void = () => {};
const track = (render: () => void): void => {
  current = render;
  render();
};

const nav: Nav = {
  index: () => track(() => void renderIndex(app, nav)),
  rune: (name) => track(() => void renderRune(app, nav, name)),
  version: (name, version) => track(() => void renderVersion(app, nav, name, version)),
  trust: () => track(() => void renderTrust(app, nav)),
};

const sessionSlot = document.getElementById("session");
function renderSession(): void {
  if (!sessionSlot) return;
  clear(sessionSlot);
  if (isSignedIn()) {
    sessionSlot.appendChild(el("span", { className: "session-id", text: "✓ signed in" }));
    sessionSlot.appendChild(button("Sign out", () => setToken(null)));
    return;
  }
  const status = el("span", { className: "muted session-status" });
  const signIn = button("Sign in with passkey", () => {
    status.textContent = "verifying passkey…";
    void passkeyLogin(location.hostname)
      .then((tok) => { setToken(tok); })
      .catch((e: Error) => { status.textContent = e.message; });
  });
  const reg = button("Register", () => {
    status.textContent = "registering passkey…";
    void registerPasskey(location.hostname)
      .then(() => { status.textContent = "passkey registered — now sign in"; })
      .catch((e: Error) => { status.textContent = "register failed: " + e.message; });
  });
  reg.classList.add("ghost");
  sessionSlot.appendChild(signIn);
  sessionSlot.appendChild(reg);
  sessionSlot.appendChild(status);
}

subscribe(() => { renderSession(); current(); });
renderSession();
nav.index();
