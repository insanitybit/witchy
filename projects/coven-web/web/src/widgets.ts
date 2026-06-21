// The two headline coven concepts as UI widgets: a rune's capability footprint
// (the authority it demands) and a version's lifecycle state.
import { el, chip } from "./dom";

export function footprintChips(fp: string[]): HTMLSpanElement {
  const wrap = el("span", { className: "chips" });
  if (!fp || fp.length === 0) {
    wrap.appendChild(chip("no authority", "cap-none"));
    return wrap;
  }
  for (const c of fp) wrap.appendChild(chip(c, "cap"));
  return wrap;
}

export function stateBadge(state: string): HTMLSpanElement {
  return chip(state, "state-" + state);
}
