// Safe DOM construction for the TRUSTED PARENT. The parent never inserts HTML
// strings at all — every node is built with createElement + textContent — so there
// is no string->HTML sink to exploit. This is the strongest form of Perfect Types
// compliance: under `trusted-types 'none'` a stray innerHTML would throw, but we
// never reach for one. (HTML insertion, e.g. highlighted source, happens only
// inside the sandbox; see sandbox.ts.)

export function clear(n: Node): void {
  while (n.firstChild) n.removeChild(n.firstChild);
}

interface ElOpts {
  className?: string;
  id?: string;
  text?: string | number;
}

export function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  opts: ElOpts = {},
): HTMLElementTagNameMap[K] {
  const n = document.createElement(tag);
  if (opts.className) n.className = opts.className;
  if (opts.id) n.id = opts.id;
  if (opts.text != null) n.textContent = String(opts.text);
  return n;
}

export function button(label: string, onClick: () => void): HTMLButtonElement {
  const b = el("button", { className: "link", text: label });
  b.addEventListener("click", onClick);
  return b;
}

export function chip(text: string, cls = ""): HTMLSpanElement {
  return el("span", { className: ("chip " + cls).trim(), text });
}
