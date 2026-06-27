// Ambient types for the glamour DOM host shell (web/witchy-runtime/glamour-dom.mjs), imported
// under the bare specifier `glamour-dom` (build.sh maps it via esbuild `--alias`). It is the
// FIXED JS host shell — the capability edge — that drives a glamour rune's MVU loop and holds
// all DOM/effect authority. Only the surface the bootstrap uses is typed here.
declare module "glamour-dom" {
  export interface MountOpts {
    initialModel?: unknown;
    routeTag?: string;
    document?: Document;
    fetch?: (url: string, init?: RequestInit) => Promise<Response>;
    authHeaders?: () => Record<string, string>;
    ports?: Record<string, (arg: string) => Promise<unknown> | unknown>;
    setTimeout?: (fn: () => void, ms: number) => unknown;
    location?: Location;
    history?: History;
  }
  export interface Mounted {
    dispatch: (msg: unknown) => void;
    getModel: () => unknown;
    unmount: () => void;
  }
  export function mount(wasmBytes: BufferSource, root: Element, opts?: MountOpts): Promise<Mounted>;
}
