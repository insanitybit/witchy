// The double-iframe sandbox host (the trusted side). Untrusted package source is
// rendered ONLY inside an opaque-origin, network-firewalled (`connect-src 'none'`)
// iframe; the parent passes data over a private MessageChannel and never receives
// anything but structured events back. See PLAN.md §5.2.
import { el } from "./dom";
import { getSourceSandboxJs } from "./api";

interface SandboxMsg {
  type: "ready" | "height";
  px?: number;
  networkBlocked?: boolean;
}

export async function mountSandbox(
  host: HTMLElement,
  sourceText: string,
  statusEl: HTMLElement,
): Promise<void> {
  let sandboxJs: string;
  try {
    sandboxJs = await getSourceSandboxJs();
  } catch (e) {
    statusEl.textContent = "sandbox failed: " + (e as Error).message;
    return;
  }

  // The inner srcdoc HTML is authored by the trusted parent and inherits the
  // sandbox CSP (script-src 'unsafe-inline'; connect-src 'none') from the outer
  // frame. The renderer source is injected as TEXT — never executed in the parent.
  const innerHtml =
    "<!doctype html><html><head><meta charset='utf-8'></head><body>" +
    "<div id='out'></div><script>" + sandboxJs + "</script></body></html>";

  const outer = el("iframe", { className: "sandbox" });
  outer.setAttribute("sandbox", "allow-scripts"); // no allow-same-origin => opaque origin
  outer.src = "/sandbox-frame";
  outer.addEventListener("load", () => {
    const win = outer.contentWindow;
    if (!win) return;
    const ch = new MessageChannel();
    ch.port1.onmessage = (e: MessageEvent<SandboxMsg>) => {
      const m = e.data;
      if (m.type === "ready") {
        ch.port1.postMessage({ type: "render", kind: "witchy-source", text: sourceText });
      } else if (m.type === "height") {
        outer.style.height = (m.px ?? 0) + 24 + "px";
        statusEl.textContent =
          "rendered inside sandbox · network firewall (connect-src 'none'): " +
          (m.networkBlocked ? "BLOCKED ✓" : "NOT blocked ✗");
      }
    };
    win.postMessage({ type: "init", html: innerHtml }, "*", [ch.port2]);
  });
  host.appendChild(outer);
}
