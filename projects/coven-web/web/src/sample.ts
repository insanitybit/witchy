// Placeholder source for the sandbox demo until /api/coven/source is wired
// (PLAN.md WS-F: the proxy must forward Unicode-capable source, which needs the
// std/server byte-length fix B1 before it is safe to enable).
export function sampleSource(name: string, version: string): string {
  return (
    "// " + name + " @ " + version + " (sample source; /api/coven/source not seeded)\n" +
    "fn dollars(cents: Int) -> Int:\n" +
    "    cents / 100\n\n" +
    "fn main(console: Console):\n" +
    "    print(console, \"$\" + \"${dollars(1299)}\")\n"
  );
}
