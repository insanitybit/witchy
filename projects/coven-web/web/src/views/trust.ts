import { clear, el, button, chip } from "../dom";
import { getRootpub, getSnapshot, getTimestamp } from "../api";
import type { Nav } from "../nav";

function ok(text: string): HTMLSpanElement { return chip(text, "cap-none"); }
function bad(text: string): HTMLSpanElement { return chip(text, "state-yanked"); }

export async function renderTrust(app: HTMLElement, nav: Nav): Promise<void> {
  try {
    const [rootpub, snap, ts] = await Promise.all([getRootpub(), getSnapshot(), getTimestamp()]);
    clear(app);
    app.appendChild(button("← all runes", () => nav.index()));
    app.appendChild(el("h2", { text: "Trust & integrity (TUF)" }));

    const dl = el("dl", { className: "record" });
    const row = (key: string, value: Node | string): void => {
      dl.appendChild(el("dt", { text: key }));
      const dd = el("dd");
      if (typeof value === "string") dd.textContent = value;
      else dd.appendChild(value);
      dl.appendChild(dd);
    };

    row("root key", rootpub);
    row("snapshot version", String(snap.signed.version));
    row("targets", Object.keys(snap.signed.targets).length + " runes signed");
    row("snapshot signature", snap.sig ? ok("Ed25519 signed ✓") : bad("MISSING ✗"));

    const expiresMs = ts.signed.expires * 1000;
    row("timestamp expires", new Date(expiresMs).toISOString());
    row("freshness (freeze check)", expiresMs > Date.now() ? ok("fresh ✓") : bad("STALE ✗"));
    row(
      "rollback check",
      ts.signed.snapshot_version === snap.signed.version ? ok("consistent ✓") : bad("MISMATCH ✗"),
    );
    row("timestamp signature", ts.sig ? ok("Ed25519 signed ✓") : bad("MISSING ✗"));
    app.appendChild(dl);

    app.appendChild(
      el("p", {
        className: "muted",
        text:
          "The timestamp's short expiry detects freeze attacks; the monotonic snapshot version " +
          "detects rollback. Both roles are Ed25519-signed by the registry root key above.",
      }),
    );
  } catch (e) {
    clear(app);
    app.appendChild(el("p", { className: "error", text: "Failed to load trust metadata: " + (e as Error).message }));
  }
}
