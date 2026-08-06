// RFC-0041: resolve a docs-bundle asset URL against the BUNDLE ROOT, not the current page.
//
// The docs app is a client-side SPA: the home page is at `/` but chapters route to
// `/p/<slug>`. A page-relative URL (`./content/x.md`, `./witchy.wasm`) resolves against the
// *current route* — from `/p/capabilities` that is `/p/content/...` / `/p/witchy.wasm`, a 404.
// Resolving against the bundle module's own URL (`import.meta.url` of `docs-boot.js`, always at
// the bundle root) is immune to the client route AND still works under a deploy subpath
// (GitHub Pages `/<repo>/`), because the module itself lives at `/<repo>/docs-boot.js`.
//
// Pure so it is unit-testable without a browser (the real bug was a base-resolution mistake the
// FakeElement drivers can't simulate).

/// A bundle asset (`witchy.wasm`, `docs.wasm`) resolved against `baseUrl` (pass
/// `import.meta.url`). Returns a `URL`.
export function assetUrl(path, baseUrl) {
  return new URL(path, baseUrl);
}

/// The docs rune emits ABSOLUTE `/content/<slug>.md` fetch URLs. Resolve them against the
/// bundle root: strip the leading `/` and join onto `baseUrl`, so the fetch never becomes
/// route-relative. A non-absolute URL is passed through unchanged. Returns a `URL` (absolute)
/// or the original value.
export function contentUrl(url, baseUrl) {
  if (typeof url === "string" && url.startsWith("/")) {
    return new URL(url.slice(1), baseUrl);
  }
  return url;
}
