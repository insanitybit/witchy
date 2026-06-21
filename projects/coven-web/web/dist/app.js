"use strict";
(() => {
  // src/dom.ts
  function clear(n) {
    while (n.firstChild) n.removeChild(n.firstChild);
  }
  function el(tag, opts = {}) {
    const n = document.createElement(tag);
    if (opts.className) n.className = opts.className;
    if (opts.id) n.id = opts.id;
    if (opts.text != null) n.textContent = String(opts.text);
    return n;
  }
  function button(label, onClick) {
    const b = el("button", { className: "link", text: label });
    b.addEventListener("click", onClick);
    return b;
  }
  function chip(text, cls = "") {
    return el("span", { className: ("chip " + cls).trim(), text });
  }

  // src/api.ts
  function wireName(n) {
    return n.replace(/\//g, "~");
  }
  async function getJSON(path) {
    const r = await fetch(path, { credentials: "omit" });
    if (!r.ok) throw new Error("HTTP " + r.status);
    return await r.json();
  }
  function getIndex() {
    return getJSON("/api/coven/index");
  }
  function getVersions(name) {
    return getJSON("/api/coven/versions?name=" + wireName(name));
  }
  function getRecord(name, version) {
    return getJSON(
      "/api/coven/record?name=" + wireName(name) + "&version=" + encodeURIComponent(version)
    );
  }
  function getSource(name, version) {
    return getJSON(
      "/api/coven/source?name=" + wireName(name) + "&version=" + encodeURIComponent(version)
    );
  }
  async function getRootpub() {
    const r = await fetch("/api/coven/rootpub", { credentials: "omit" });
    if (!r.ok) throw new Error("HTTP " + r.status);
    return (await r.text()).trim();
  }
  function getSnapshot() {
    return getJSON("/api/coven/snapshot");
  }
  function getTimestamp() {
    return getJSON("/api/coven/timestamp");
  }
  async function postJSON(path, body) {
    const r = await fetch(path, {
      method: "POST",
      credentials: "omit",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(body)
    });
    const data = await r.json();
    if (!r.ok) throw new Error(data.error ?? "HTTP " + r.status);
    return data;
  }
  function promote(name, version, secondFactor, promotedBy) {
    return postJSON("/api/coven/promote", {
      name,
      version,
      second_factor: secondFactor,
      promoted_by: promotedBy
    });
  }
  function yank(name, version) {
    return postJSON("/api/coven/yank", { name, version });
  }
  async function getSourceSandboxJs() {
    const r = await fetch("/source-sandbox.js", { credentials: "omit" });
    if (!r.ok) throw new Error("HTTP " + r.status);
    return r.text();
  }

  // src/sandbox.ts
  async function mountSandbox(host, sourceText, statusEl) {
    let sandboxJs;
    try {
      sandboxJs = await getSourceSandboxJs();
    } catch (e) {
      statusEl.textContent = "sandbox failed: " + e.message;
      return;
    }
    const innerHtml = "<!doctype html><html><head><meta charset='utf-8'></head><body><div id='out'></div><script>" + sandboxJs + "<\/script></body></html>";
    const outer = el("iframe", { className: "sandbox" });
    outer.setAttribute("sandbox", "allow-scripts");
    outer.src = "/sandbox-frame";
    outer.addEventListener("load", () => {
      const win = outer.contentWindow;
      if (!win) return;
      const ch = new MessageChannel();
      ch.port1.onmessage = (e) => {
        const m = e.data;
        if (m.type === "ready") {
          ch.port1.postMessage({ type: "render", kind: "witchy-source", text: sourceText });
        } else if (m.type === "height") {
          outer.style.height = (m.px ?? 0) + 24 + "px";
          statusEl.textContent = "rendered inside sandbox \xB7 network firewall (connect-src 'none'): " + (m.networkBlocked ? "BLOCKED \u2713" : "NOT blocked \u2717");
        }
      };
      win.postMessage({ type: "init", html: innerHtml }, "*", [ch.port2]);
    });
    host.appendChild(outer);
  }

  // src/sample.ts
  function sampleSource(name, version) {
    return "// " + name + " @ " + version + ' (sample source; /api/coven/source not seeded)\nfn dollars(cents: Int) -> Int:\n    cents / 100\n\nfn main(console: Console):\n    print(console, "$" + "${dollars(1299)}")\n';
  }

  // src/views/index.ts
  async function renderIndex(app2, nav2) {
    try {
      const { names } = await getIndex();
      clear(app2);
      const topnav = el("div", { className: "topnav" });
      topnav.appendChild(button("Trust & integrity (TUF) \u2192", () => nav2.trust()));
      app2.appendChild(topnav);
      app2.appendChild(el("h2", { text: "Runes" }));
      const search = el("input", { className: "field search" });
      search.setAttribute("type", "search");
      search.setAttribute("placeholder", "filter runes\u2026");
      search.setAttribute("aria-label", "filter runes");
      app2.appendChild(search);
      const ul = el("ul", { className: "rune-list" });
      app2.appendChild(ul);
      const note = el("p", { className: "muted" });
      app2.appendChild(note);
      const renderList = (filter) => {
        clear(ul);
        const f = filter.trim().toLowerCase();
        const shown = names.filter((n) => n.toLowerCase().includes(f));
        note.textContent = shown.length ? "" : names.length ? "No runes match \u201C" + filter + "\u201D." : "No runes published yet.";
        for (const n of shown) {
          const li = el("li");
          li.appendChild(button(n, () => nav2.rune(n)));
          ul.appendChild(li);
        }
      };
      search.addEventListener("input", () => renderList(search.value));
      renderList("");
      app2.appendChild(el("h2", { text: "Source viewer (sandbox demo)" }));
      const status = el("p", { className: "muted", id: "sandbox-status", text: "loading sandbox\u2026" });
      app2.appendChild(status);
      const box = el("div", { className: "sandbox-box" });
      app2.appendChild(box);
      void mountSandbox(box, sampleSource("acme/money", "2.0.0"), status);
    } catch (e) {
      clear(app2);
      app2.appendChild(el("p", { className: "error", text: "Failed to load registry: " + e.message }));
    }
  }

  // src/widgets.ts
  function footprintChips(fp) {
    const wrap = el("span", { className: "chips" });
    if (!fp || fp.length === 0) {
      wrap.appendChild(chip("no authority", "cap-none"));
      return wrap;
    }
    for (const c of fp) wrap.appendChild(chip(c, "cap"));
    return wrap;
  }
  function stateBadge(state) {
    return chip(state, "state-" + state);
  }

  // src/views/rune.ts
  async function renderRune(app2, nav2, name) {
    try {
      const { records } = await getVersions(name);
      clear(app2);
      app2.appendChild(button("\u2190 all runes", () => nav2.index()));
      app2.appendChild(el("h2", { text: name }));
      const recs = records.filter(Boolean);
      if (recs.length === 0) {
        app2.appendChild(el("p", { text: "No versions." }));
        return;
      }
      const ul = el("ul", { className: "version-list" });
      for (const r of recs) {
        const li = el("li");
        const head = el("div", { className: "version-head" });
        head.appendChild(button(r.version, () => nav2.version(name, r.version)));
        head.appendChild(stateBadge(r.state));
        li.appendChild(head);
        li.appendChild(footprintChips(r.runtime_footprint));
        ul.appendChild(li);
      }
      app2.appendChild(ul);
    } catch (e) {
      clear(app2);
      app2.appendChild(el("p", { className: "error", text: "Failed to load versions: " + e.message }));
    }
  }

  // src/webauthn.ts
  function hex(buf) {
    return Array.from(new Uint8Array(buf)).map((b) => b.toString(16).padStart(2, "0")).join("");
  }
  function hexToBytes(h) {
    const out = new Uint8Array(h.length / 2);
    for (let i = 0; i < out.length; i++) out[i] = parseInt(h.slice(i * 2, i * 2 + 2), 16);
    return out;
  }
  function b64url(buf) {
    let s = "";
    const b = new Uint8Array(buf);
    for (let i = 0; i < b.length; i++) s += String.fromCharCode(b[i]);
    return btoa(s).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
  }
  function spkiToSec1Hex(spki) {
    const b = new Uint8Array(spki);
    return hex(b.slice(b.length - 65).buffer);
  }
  async function challengeBytes() {
    const r = await fetch("/api/webauthn/challenge", { credentials: "omit" });
    const { challengeHex } = await r.json();
    return hexToBytes(challengeHex);
  }
  async function register(rpId) {
    const cred = await navigator.credentials.create({
      publicKey: {
        challenge: await challengeBytes(),
        rp: { id: rpId, name: "coven" },
        user: { id: new Uint8Array([1]), name: "maintainer", displayName: "maintainer" },
        pubKeyCredParams: [{ type: "public-key", alg: -7 }],
        // A discoverable (resident) passkey, so the assertion can find it without the
        // server having to hand back an allow-list of credential ids.
        authenticatorSelection: { residentKey: "required", requireResidentKey: true, userVerification: "required" },
        timeout: 6e4
      }
    });
    const resp = cred.response;
    const spki = resp.getPublicKey();
    if (!spki) throw new Error("authenticator returned no public key");
    await fetch("/api/webauthn/register", {
      method: "POST",
      credentials: "omit",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ credentialId: b64url(cred.rawId), publicKey: spkiToSec1Hex(spki) })
    });
  }
  async function promote2fa(rpId, name, version, promotedBy) {
    const assertion = await navigator.credentials.get({
      publicKey: {
        challenge: await challengeBytes(),
        rpId,
        userVerification: "required",
        timeout: 6e4
      }
    });
    const resp = assertion.response;
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
        signature: hex(resp.signature)
      })
    });
  }

  // src/views/version.ts
  function labeledField(label, control) {
    const w = el("label", { className: "labeled" });
    w.appendChild(el("span", { className: "muted", text: label + ": " }));
    w.appendChild(control);
    return w;
  }
  function renderWriteControls(app2, nav2, name, version, state) {
    if (state === "staged") {
      app2.appendChild(el("h2", { text: "Promote \u2014 2FA to publish" }));
      const wrap = el("div", { className: "promote" });
      const factor = el("input", { className: "field" });
      factor.value = "totp-000000";
      factor.setAttribute("aria-label", "second factor");
      const who = el("input", { className: "field" });
      who.value = "maintainer@demo";
      who.setAttribute("aria-label", "promoter identity (must differ from uploader)");
      const status = el("p", { className: "muted" });
      const go = button("Promote \u2192 released", () => {
        status.textContent = "promoting\u2026";
        void promote(name, version, factor.value, who.value).then((res) => {
          status.textContent = "released \u2713 \xB7 separation of duties: " + (res.separation_of_duties ? "yes" : "no") + " \xB7 newly exposed authority: " + (res.delta_runtime.length ? res.delta_runtime.join(", ") : "none");
          setTimeout(() => nav2.version(name, version), 700);
        }).catch((e) => {
          status.textContent = "refused: " + e.message;
        });
      });
      wrap.appendChild(labeledField("second factor", factor));
      wrap.appendChild(labeledField("promoter", who));
      wrap.appendChild(go);
      app2.appendChild(wrap);
      app2.appendChild(status);
      const pkWrap = el("div", { className: "promote" });
      const pkStatus = el("p", { className: "muted" });
      pkWrap.appendChild(
        button("Register passkey", () => {
          pkStatus.textContent = "registering passkey\u2026";
          void register(location.hostname).then(() => {
            pkStatus.textContent = "passkey registered \u2713";
          }).catch((e) => {
            pkStatus.textContent = "register failed: " + e.message;
          });
        })
      );
      pkWrap.appendChild(
        button("Promote with passkey (2FA)", () => {
          pkStatus.textContent = "verifying passkey\u2026";
          void promote2fa(location.hostname, name, version, who.value).then(async (r) => {
            if (r.ok) {
              pkStatus.textContent = "released \u2713 (passkey 2FA)";
              setTimeout(() => nav2.version(name, version), 700);
            } else {
              const d = await r.json().catch(() => ({}));
              pkStatus.textContent = "refused: " + (d.error ?? r.status);
            }
          }).catch((e) => {
            pkStatus.textContent = "refused: " + e.message;
          });
        })
      );
      app2.appendChild(pkWrap);
      app2.appendChild(pkStatus);
    } else if (state === "released") {
      const status = el("p", { className: "muted" });
      const yk = button("Yank this version", () => {
        status.textContent = "yanking\u2026";
        void yank(name, version).then(() => {
          status.textContent = "yanked \u2713";
          setTimeout(() => nav2.version(name, version), 700);
        }).catch((e) => {
          status.textContent = "refused: " + e.message;
        });
      });
      app2.appendChild(yk);
      app2.appendChild(status);
    }
  }
  async function renderVersion(app2, nav2, name, version) {
    try {
      const r = await getRecord(name, version);
      clear(app2);
      app2.appendChild(button("\u2190 " + name, () => nav2.rune(name)));
      app2.appendChild(el("h2", { text: name + " @ " + version }));
      const dl = el("dl", { className: "record" });
      const row = (key, value) => {
        dl.appendChild(el("dt", { text: key }));
        const dd = el("dd");
        if (value && typeof value === "object" && "nodeType" in value) dd.appendChild(value);
        else dd.textContent = value === null || value === "" ? "\u2014" : String(value);
        dl.appendChild(dd);
      };
      row("state", stateBadge(r.state));
      row("runtime footprint", footprintChips(r.runtime_footprint));
      row("determinism", r.determinism);
      row("hash", r.hash);
      row("uploaded by", r.uploaded_by);
      row("promoted by", r.promoted_by);
      row("second factor", r.second_factor);
      row("provenance", r.provenance);
      row("released", r.released_at ? new Date(r.released_at * 1e3).toISOString() : null);
      row("signature", r.sig);
      app2.appendChild(dl);
      renderWriteControls(app2, nav2, name, version, r.state);
      app2.appendChild(el("h2", { text: "Source (sandbox)" }));
      const status = el("p", { className: "muted", id: "sandbox-status", text: "loading source\u2026" });
      app2.appendChild(status);
      const filebar = el("div", { className: "filebar" });
      app2.appendChild(filebar);
      const box = el("div", { className: "sandbox-box" });
      app2.appendChild(box);
      try {
        const { files } = await getSource(name, version);
        if (files.length === 0) {
          status.textContent = "no source files";
        } else {
          const buttons = files.map(([path], i) => {
            const b = button(path, () => show(i));
            filebar.appendChild(b);
            return b;
          });
          const show = (idx) => {
            buttons.forEach((b, i) => b.classList.toggle("active", i === idx));
            const file = files[idx];
            if (file) void mountSandbox(box, file[1], status);
          };
          const srcFirst = files.findIndex(([p]) => p.startsWith("src/"));
          show(srcFirst >= 0 ? srcFirst : 0);
        }
      } catch (e) {
        status.textContent = "source unavailable for this version: " + e.message;
      }
    } catch (e) {
      clear(app2);
      app2.appendChild(el("p", { className: "error", text: "Failed to load record: " + e.message }));
    }
  }

  // src/views/trust.ts
  function ok(text) {
    return chip(text, "cap-none");
  }
  function bad(text) {
    return chip(text, "state-yanked");
  }
  async function renderTrust(app2, nav2) {
    try {
      const [rootpub, snap, ts] = await Promise.all([getRootpub(), getSnapshot(), getTimestamp()]);
      clear(app2);
      app2.appendChild(button("\u2190 all runes", () => nav2.index()));
      app2.appendChild(el("h2", { text: "Trust & integrity (TUF)" }));
      const dl = el("dl", { className: "record" });
      const row = (key, value) => {
        dl.appendChild(el("dt", { text: key }));
        const dd = el("dd");
        if (typeof value === "string") dd.textContent = value;
        else dd.appendChild(value);
        dl.appendChild(dd);
      };
      row("root key", rootpub);
      row("snapshot version", String(snap.signed.version));
      row("targets", Object.keys(snap.signed.targets).length + " runes signed");
      row("snapshot signature", snap.sig ? ok("Ed25519 signed \u2713") : bad("MISSING \u2717"));
      const expiresMs = ts.signed.expires * 1e3;
      row("timestamp expires", new Date(expiresMs).toISOString());
      row("freshness (freeze check)", expiresMs > Date.now() ? ok("fresh \u2713") : bad("STALE \u2717"));
      row(
        "rollback check",
        ts.signed.snapshot_version === snap.signed.version ? ok("consistent \u2713") : bad("MISMATCH \u2717")
      );
      row("timestamp signature", ts.sig ? ok("Ed25519 signed \u2713") : bad("MISSING \u2717"));
      app2.appendChild(dl);
      app2.appendChild(
        el("p", {
          className: "muted",
          text: "The timestamp's short expiry detects freeze attacks; the monotonic snapshot version detects rollback. Both roles are Ed25519-signed by the registry root key above."
        })
      );
    } catch (e) {
      clear(app2);
      app2.appendChild(el("p", { className: "error", text: "Failed to load trust metadata: " + e.message }));
    }
  }

  // src/main.ts
  var app = document.getElementById("app");
  if (!app) throw new Error("coven-web: missing #app element");
  var nav = {
    index: () => void renderIndex(app, nav),
    rune: (name) => void renderRune(app, nav, name),
    version: (name, version) => void renderVersion(app, nav, name, version),
    trust: () => void renderTrust(app, nav)
  };
  nav.index();
})();
//# sourceMappingURL=app.js.map
