// witchy-highlight — the ONE dependency-free witchy syntax highlighter (RFC-0041 Phase 0).
// A data-only `source -> HTML` function (no DOM), so it is shared by every web surface — the
// standalone playground and the docs app's code blocks — and unit-tested in Node. Promoting
// this to a single module is what keeps the keyword/builtin lists in ONE place instead of
// drifting between `web/playground.js` and the book's highlighter.
//
// It emits `<span class="t-*">` tokens over HTML-escaped text: comments, strings (with
// `${…}` interpolation coloured distinctly), numbers (incl. duration suffixes), then words
// classified keyword / literal / builtin / type (Title-case) / plain.

export const KEYWORDS = new Set(
  ("fn let var if else match for in while return break continue type trait " +
    "impl actor on import pub own move spawn where as gen yield " +
    "retain without capability grantable mut mode async await sealed")
    .split(" "),
);

export const LITERALS = new Set(["true", "false"]);

// Capability-op + core builtins the language still exposes bare (module-qualified stdlib
// like `string.length` colours the method word too — harmless). The retired `restrict`
// and `subdir` verbs (RFC-0011: narrowing is `net.only`/`dir.subtree`) are intentionally
// absent so the highlighter never paints a keyword the compiler rejects.
export const BUILTINS = new Set(
  ("print read write append exists is_dir list subtree make_dir read_file write_file " +
    "connect try_connect listen accept send_line send_bytes recv_line recv_all recv_bytes " +
    "close only deny now get_env send spawn length at push concat step_with fail " +
    "int_to_string string_length to_string")
    .split(" "),
);

export function escapeHtml(s) {
  return s.replace(/[&<>]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;" })[c]);
}

// One pass: comments, strings (with ${...} interpolation), numbers (incl. duration
// suffixes), lifetimes, then identifier words. Lifetimes are kept as one token so
// explicit reference types such as `&'a mut String` are recognizable without
// changing the source-preserving nature of this highlighter.
const TOKEN =
  /(\/\/[^\n]*|\/\*[\s\S]*?\*\/)|("(?:\\.|[^"\\])*")|(\b\d+(?:\.\d+)?(?:ms|hr|s|m|h|d|w)?\b)|('[A-Za-z_][A-Za-z0-9_]*)|([A-Za-z_][A-Za-z0-9_]*)/g;

function highlightString(str) {
  // Colour ${...} interpolations distinctly inside an otherwise-green string.
  let out = "";
  let last = 0;
  const re = /\$\{[^}]*\}/g;
  let m;
  while ((m = re.exec(str))) {
    out += `<span class="t-str">${escapeHtml(str.slice(last, m.index))}</span>`;
    out += `<span class="t-subst">${escapeHtml(m[0])}</span>`;
    last = re.lastIndex;
  }
  out += `<span class="t-str">${escapeHtml(str.slice(last))}</span>`;
  return out;
}

export function highlightWitchy(src) {
  let out = "";
  let last = 0;
  let m;
  TOKEN.lastIndex = 0;
  while ((m = TOKEN.exec(src))) {
    out += escapeHtml(src.slice(last, m.index));
    last = TOKEN.lastIndex;
    if (m[1]) out += `<span class="t-comment">${escapeHtml(m[1])}</span>`;
    else if (m[2]) out += highlightString(m[2]);
    else if (m[3]) out += `<span class="t-num">${escapeHtml(m[3])}</span>`;
    else if (m[4]) out += `<span class="t-type">${escapeHtml(m[4])}</span>`;
    else {
      const w = m[5];
      const cls = KEYWORDS.has(w)
        ? "t-kw"
        : LITERALS.has(w)
          ? "t-lit"
          : BUILTINS.has(w)
            ? "t-builtin"
            : /^[A-Z]/.test(w)
              ? "t-type"
              : null;
      out += cls ? `<span class="${cls}">${escapeHtml(w)}</span>` : escapeHtml(w);
    }
  }
  out += escapeHtml(src.slice(last));
  return out;
}

// A small SHELL highlighter for the book's `sh` fences (`witchy sandbox …`, `cargo …`). Emits
// the same `t-*` token classes as `highlightWitchy` (so it reuses the theme) over HTML-escaped
// text — comments, strings, `$vars`, and `-x`/`--flag` options.
export function highlightShell(src) {
  const TOKEN = /(#[^\n]*)|("(?:\\.|[^"\\])*"|'[^']*')|(\$\{?\w+\}?)|(-{1,2}[A-Za-z][\w-]*)/g;
  let out = "", last = 0, m;
  TOKEN.lastIndex = 0;
  while ((m = TOKEN.exec(src))) {
    out += escapeHtml(src.slice(last, m.index));
    last = TOKEN.lastIndex;
    if (m[1]) out += `<span class="t-comment">${escapeHtml(m[1])}</span>`;
    else if (m[2]) out += `<span class="t-str">${escapeHtml(m[2])}</span>`;
    else if (m[3]) out += `<span class="t-subst">${escapeHtml(m[3])}</span>`;
    else if (m[4]) out += `<span class="t-kw">${escapeHtml(m[4])}</span>`;
  }
  out += escapeHtml(src.slice(last));
  return out;
}

// A small TOML highlighter for the book's `toml` fences (`witchy.toml`). Same `t-*` classes,
// HTML-escaped — comments, `[sections]`, `key =`, strings, booleans, numbers.
export function highlightToml(src) {
  const TOKEN =
    /(#[^\n]*)|(^\s*\[[^\]\n]*\])|(^\s*[A-Za-z_][\w.-]*)(?=\s*=)|("(?:\\.|[^"\\])*"|'[^']*')|(\btrue\b|\bfalse\b)|(\b\d[\d._:T+-]*\b)/gm;
  let out = "", last = 0, m;
  TOKEN.lastIndex = 0;
  while ((m = TOKEN.exec(src))) {
    out += escapeHtml(src.slice(last, m.index));
    last = TOKEN.lastIndex;
    if (m[1]) out += `<span class="t-comment">${escapeHtml(m[1])}</span>`;
    else if (m[2]) out += `<span class="t-type">${escapeHtml(m[2])}</span>`;
    else if (m[3]) out += `<span class="t-builtin">${escapeHtml(m[3])}</span>`;
    else if (m[4]) out += `<span class="t-str">${escapeHtml(m[4])}</span>`;
    else if (m[5]) out += `<span class="t-lit">${escapeHtml(m[5])}</span>`;
    else if (m[6]) out += `<span class="t-num">${escapeHtml(m[6])}</span>`;
  }
  out += escapeHtml(src.slice(last));
  return out;
}
