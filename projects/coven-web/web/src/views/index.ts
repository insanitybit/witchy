import { clear, el, button } from "../dom";
import { getIndex } from "../api";
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

    app.appendChild(el("h2", { text: "Source viewer (sandbox demo)" }));
    const status = el("p", { className: "muted", id: "sandbox-status", text: "loading sandbox…" });
    app.appendChild(status);
    const box = el("div", { className: "sandbox-box" });
    app.appendChild(box);
    void mountSandbox(box, sampleSource("acme/money", "2.0.0"), status);
  } catch (e) {
    clear(app);
    app.appendChild(el("p", { className: "error", text: "Failed to load registry: " + (e as Error).message }));
  }
}
