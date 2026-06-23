import { clear, el, button } from "../dom";
import { getIndex, getVersions, getSource } from "../api";
import { mountSandbox } from "../sandbox";
import { sampleSource } from "../sample";
import type { Nav } from "../nav";

export async function renderIndex(app: HTMLElement, nav: Nav): Promise<void> {
  try {
    const { names } = await getIndex();
    clear(app);
    const topnav = el("div", { className: "topnav" });
    topnav.appendChild(button("Trust & integrity (TUF) →", () => nav.trust()));
    app.appendChild(topnav);
    app.appendChild(el("h2", { text: "Runes" }));

    const search = el("input", { className: "field search" });
    search.setAttribute("type", "search");
    search.setAttribute("placeholder", "filter runes…");
    search.setAttribute("aria-label", "filter runes");
    app.appendChild(search);

    const ul = el("ul", { className: "rune-list" });
    app.appendChild(ul);
    const note = el("p", { className: "muted" });
    app.appendChild(note);

    const renderList = (filter: string): void => {
      clear(ul);
      const f = filter.trim().toLowerCase();
      const shown = names.filter((n) => n.toLowerCase().includes(f));
      note.textContent = shown.length
        ? ""
        : names.length
          ? "No runes match “" + filter + "”."
          : "No runes published yet.";
      for (const n of shown) {
        const li = el("li");
        li.appendChild(button(n, () => nav.rune(n)));
        ul.appendChild(li);
      }
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
    void mountDemoSource(names, caption, box, status);
  } catch (e) {
    clear(app);
    app.appendChild(el("p", { className: "error", text: "Failed to load registry: " + (e as Error).message }));
  }
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
