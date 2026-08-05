// Hash routing keeps the static book refreshable on hosts without an SPA
// fallback. Glamour sees ordinary paths; only this adapter knows about `#`.

export function createHashRouting(browserWindow) {
  const path = () => {
    const hash = browserWindow.location.hash;
    return hash && hash.length > 1 ? hash.slice(1) : "/";
  };

  const programmatic = new Set();
  const history = {
    pushState: (_state, _title, nextPath) => {
      const nextHash = `#${nextPath}`;
      if (browserWindow.location.hash === nextHash) return;
      programmatic.add(nextHash);
      browserWindow.location.hash = nextPath;
    },
  };
  const location = {
    get pathname() {
      return path();
    },
  };
  const onPopState = (fire) => {
    const onHashChange = (event) => {
      const changedHash = event?.newURL
        ? new URL(event.newURL).hash
        : browserWindow.location.hash;
      if (programmatic.delete(changedHash)) return;
      fire();
    };
    browserWindow.addEventListener("hashchange", onHashChange);
    return () => browserWindow.removeEventListener("hashchange", onHashChange);
  };

  return { path, history, location, onPopState };
}
