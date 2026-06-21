import { clear, el, button } from "../dom";
import { getRecord, getSource, promote, yank } from "../api";
import { footprintChips, stateBadge } from "../widgets";
import { mountSandbox } from "../sandbox";
import { register as registerPasskey, promote2fa } from "../webauthn";
import type { Nav } from "../nav";

function labeledField(label: string, control: HTMLElement): HTMLElement {
  const w = el("label", { className: "labeled" });
  w.appendChild(el("span", { className: "muted", text: label + ": " }));
  w.appendChild(control);
  return w;
}

function renderWriteControls(app: HTMLElement, nav: Nav, name: string, version: string, state: string): void {
  if (state === "staged") {
    app.appendChild(el("h2", { text: "Promote — 2FA to publish" }));
    const wrap = el("div", { className: "promote" });
    const factor = el("input", { className: "field" });
    factor.value = "totp-000000";
    factor.setAttribute("aria-label", "second factor");
    const who = el("input", { className: "field" });
    who.value = "maintainer@demo";
    who.setAttribute("aria-label", "promoter identity (must differ from uploader)");
    const status = el("p", { className: "muted" });
    const go = button("Promote → released", () => {
      status.textContent = "promoting…";
      void promote(name, version, factor.value, who.value)
        .then((res) => {
          status.textContent =
            "released ✓ · separation of duties: " + (res.separation_of_duties ? "yes" : "no") +
            " · newly exposed authority: " +
            (res.delta_runtime.length ? res.delta_runtime.join(", ") : "none");
          setTimeout(() => nav.version(name, version), 700);
        })
        .catch((e: Error) => { status.textContent = "refused: " + e.message; });
    });
    wrap.appendChild(labeledField("second factor", factor));
    wrap.appendChild(labeledField("promoter", who));
    wrap.appendChild(go);
    app.appendChild(wrap);
    app.appendChild(status);

    // Passkey (WebAuthn ES256) second factor — verified server-side via std/webauthn.
    const pkWrap = el("div", { className: "promote" });
    const pkStatus = el("p", { className: "muted" });
    pkWrap.appendChild(
      button("Register passkey", () => {
        pkStatus.textContent = "registering passkey…";
        void registerPasskey(location.hostname)
          .then(() => { pkStatus.textContent = "passkey registered ✓"; })
          .catch((e: Error) => { pkStatus.textContent = "register failed: " + e.message; });
      }),
    );
    pkWrap.appendChild(
      button("Promote with passkey (2FA)", () => {
        pkStatus.textContent = "verifying passkey…";
        void promote2fa(location.hostname, name, version, who.value)
          .then(async (r) => {
            if (r.ok) {
              pkStatus.textContent = "released ✓ (passkey 2FA)";
              setTimeout(() => nav.version(name, version), 700);
            } else {
              const d = (await r.json().catch(() => ({}))) as { error?: string };
              pkStatus.textContent = "refused: " + (d.error ?? r.status);
            }
          })
          .catch((e: Error) => { pkStatus.textContent = "refused: " + e.message; });
      }),
    );
    app.appendChild(pkWrap);
    app.appendChild(pkStatus);
  } else if (state === "released") {
    const status = el("p", { className: "muted" });
    const yk = button("Yank this version", () => {
      status.textContent = "yanking…";
      void yank(name, version)
        .then(() => { status.textContent = "yanked ✓"; setTimeout(() => nav.version(name, version), 700); })
        .catch((e: Error) => { status.textContent = "refused: " + e.message; });
    });
    app.appendChild(yk);
    app.appendChild(status);
  }
}

export async function renderVersion(
  app: HTMLElement,
  nav: Nav,
  name: string,
  version: string,
): Promise<void> {
  try {
    const r = await getRecord(name, version);
    clear(app);
    app.appendChild(button("← " + name, () => nav.rune(name)));
    app.appendChild(el("h2", { text: name + " @ " + version }));

    const dl = el("dl", { className: "record" });
    const row = (key: string, value: Node | string | number | null): void => {
      dl.appendChild(el("dt", { text: key }));
      const dd = el("dd");
      if (value && typeof value === "object" && "nodeType" in value) dd.appendChild(value);
      else dd.textContent = value === null || value === "" ? "—" : String(value);
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
    row("released", r.released_at ? new Date(r.released_at * 1000).toISOString() : null);
    row("signature", r.sig);
    app.appendChild(dl);

    renderWriteControls(app, nav, name, version, r.state);

    app.appendChild(el("h2", { text: "Source (sandbox)" }));
    const status = el("p", { className: "muted", id: "sandbox-status", text: "loading source…" });
    app.appendChild(status);
    const filebar = el("div", { className: "filebar" });
    app.appendChild(filebar);
    const box = el("div", { className: "sandbox-box" });
    app.appendChild(box);
    // The untrusted package source is fetched and rendered ONLY inside the sandbox.
    // A per-file selector (PLAN §7.4) renders the chosen file; switching files re-mounts
    // a fresh sandbox with just that file's bytes.
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
        const show = (idx: number): void => {
          buttons.forEach((b, i) => b.classList.toggle("active", i === idx));
          const file = files[idx];
          if (file) void mountSandbox(box, file[1], status);
        };
        const srcFirst = files.findIndex(([p]) => p.startsWith("src/"));
        show(srcFirst >= 0 ? srcFirst : 0);
      }
    } catch (e) {
      status.textContent = "source unavailable for this version: " + (e as Error).message;
    }
  } catch (e) {
    clear(app);
    app.appendChild(el("p", { className: "error", text: "Failed to load record: " + (e as Error).message }));
  }
}
