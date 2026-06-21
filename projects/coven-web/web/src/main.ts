// coven-web client entry. Zero runtime dependencies. The trusted parent builds
// DOM with createElement/textContent only (see dom.ts) and never inserts HTML
// strings, so Perfect Types (`trusted-types 'none'`) has nothing to catch here.
import { renderIndex } from "./views/index";
import { renderRune } from "./views/rune";
import { renderVersion } from "./views/version";
import { renderTrust } from "./views/trust";
import type { Nav } from "./nav";

const app = document.getElementById("app");
if (!app) throw new Error("coven-web: missing #app element");

const nav: Nav = {
  index: () => void renderIndex(app, nav),
  rune: (name) => void renderRune(app, nav, name),
  version: (name, version) => void renderVersion(app, nav, name, version),
  trust: () => void renderTrust(app, nav),
};

nav.index();
