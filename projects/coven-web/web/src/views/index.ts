import { clear, el, button } from "../dom";
import { getCatalog, getVersions, getSource } from "../api";
import { mountSandbox } from "../sandbox";
import { sampleSource } from "../sample";
import type { Nav } from "../nav";
import type { CatalogEntry } from "../types";

export async function renderIndex(app: HTMLElement, nav: Nav): Promise<void> {
  try {
    const { runes } = await getCatalog();
    clear(app);
    const topnav = el("div", { className: "topnav" });
    topnav.appendChild(button("Trust & integrity (TUF) →", () => nav.trust()));
    app.appendChild(topnav);
    app.appendChild(el("h2", { text: "Runes" }));

    const search = el("input", { className: "field search" });
    search.setAttribute("type", "search");
    search.setAttribute("placeholder", "filter by name or capability…");
    search.setAttribute("aria-label", "filter runes by name or capability");
    app.appendChild(search);

    const note = el("p", { className: "muted count" });
    app.appendChild(note);
    const ul = el("ul", { className: "rune-list" });
    app.appendChild(ul);

    const renderList = (filter: string): void => {
      clear(ul);
      const f = filter.trim().toLowerCase();
      // Search matches the rune name OR any capability in its footprint, so the
      // box doubles as capability discovery ("which runes touch the network?").
      const shown = runes.filter(
        (r) =>
          r.name.toLowerCase().includes(f) ||
          r.runtime_footprint.some((c) => c.toLowerCase().includes(f)),
      );
      note.textContent = runes.length === 0
        ? "No runes published yet."
        : shown.length === runes.length
          ? runes.length + " runes"
          : shown.length + " of " + runes.length + " runes";
      for (const r of shown) ul.appendChild(runeRow(r, nav));
    };
    search.addEventListener("input", () => renderList(search.value));
    renderList("");

    app.appendChild(el("h2", { text: "Source viewer (sandboxed)" }));
    const caption = el("p", { className: "muted", text: "loading a published rune…" });
    app.appendChild(caption);
    const status = el("p", { className: "muted", id: "sandbox-status", text: "loading sandbox…" });
    app.appendChild(status);
    const box = el("div", { className: "sandbox-box" });
    app.appendChild(box);
    void mountDemoSource(runes.map((r) => r.name), caption, box, status);
  } catch (e) {
    clear(app);
    app.appendChild(el("p", { className: "error", text: "Failed to load registry: " + (e as Error).message }));
  }
}

// One rune as a card: name + version (state-colored) on top, its capability
// footprint below — gold cap chips, or a green "no authority" badge for the
// celebrated empty footprint. Authority is the headline, shown at a glance.
function runeRow(r: CatalogEntry, nav: Nav): HTMLElement {
  const li = el("li", { className: "rune-row" });
  const head = el("div", { className: "rune-row-head" });
  head.appendChild(button(r.name, () => nav.rune(r.name)));
  head.appendChild(el("span", { className: "chip state-" + r.state, text: r.version + " · " + r.state }));
  li.appendChild(head);

  const caps = el("div", { className: "chips caps" });
  if (r.runtime_footprint.length === 0) {
    caps.appendChild(el("span", { className: "chip cap-none", text: "no authority" }));
  } else {
    for (const c of r.runtime_footprint) caps.appendChild(el("span", { className: "chip cap", text: c }));
  }
  li.appendChild(caps);
  return li;
}

// Render a REAL published rune's source in the sandbox as the landing showcase —
// the same opaque-origin, network-firewalled frame the version page uses, so the
// front page proves the pipeline end-to-end on live, hash-verified data. Falls
// back to a static sample only if the registry is empty or no source loads.
async function mountDemoSource(
  names: string[],
  caption: HTMLElement,
  box: HTMLElement,
  status: HTMLElement,
): Promise<void> {
  for (const name of names) {
    try {
      const { records } = await getVersions(name);
      const rec = records.find((r) => r.state === "released") ?? records[0];
      if (!rec) continue;
      const { files } = await getSource(name, rec.version);
      const file = files.find(([p]) => p.startsWith("src/")) ?? files[0];
      if (!file) continue;
      caption.textContent = name + " @ " + rec.version + " · " + file[0];
      void mountSandbox(box, file[1], status);
      return;
    } catch {
      // Registry hiccup for this rune — try the next one.
    }
  }
  caption.textContent = "no published source yet — showing a static sample";
  void mountSandbox(box, sampleSource("acme/money", "2.0.0"), status);
}
