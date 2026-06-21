import { clear, el, button } from "../dom";
import { getVersions } from "../api";
import { footprintChips, stateBadge } from "../widgets";
import type { Nav } from "../nav";

export async function renderRune(app: HTMLElement, nav: Nav, name: string): Promise<void> {
  try {
    const { records } = await getVersions(name);
    clear(app);
    app.appendChild(button("← all runes", () => nav.index()));
    app.appendChild(el("h2", { text: name }));

    const recs = records.filter(Boolean);
    if (recs.length === 0) {
      app.appendChild(el("p", { text: "No versions." }));
      return;
    }
    const ul = el("ul", { className: "version-list" });
    for (const r of recs) {
      const li = el("li");
      const head = el("div", { className: "version-head" });
      head.appendChild(button(r.version, () => nav.version(name, r.version)));
      head.appendChild(stateBadge(r.state));
      li.appendChild(head);
      li.appendChild(footprintChips(r.runtime_footprint));
      ul.appendChild(li);
    }
    app.appendChild(ul);
  } catch (e) {
    clear(app);
    app.appendChild(el("p", { className: "error", text: "Failed to load versions: " + (e as Error).message }));
  }
}
