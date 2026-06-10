// The witchy playground: load the interpreter compiled to wasm and run snippets
// entirely in the browser. The module exports a tiny C ABI (see src/lib.rs):
// witchy_alloc / witchy_free for marshaling, and witchy_run(ptr, len) which
// returns a pointer to `[u32 length][utf-8 bytes]` whose first line is the tag
// (`ok` / `error`) and the rest is the program output or the error message.

let wasm = null;

async function loadWitchy() {
  const resp = await fetch("witchy.wasm");
  const bytes = await resp.arrayBuffer();
  const { instance } = await WebAssembly.instantiate(bytes, {});
  wasm = instance.exports;
}

function runWitchy(source) {
  const { memory, witchy_alloc, witchy_run, witchy_free } = wasm;
  const enc = new TextEncoder().encode(source);
  const ptr = witchy_alloc(enc.length);
  new Uint8Array(memory.buffer, ptr, enc.length).set(enc);
  const resPtr = witchy_run(ptr, enc.length);
  witchy_free(ptr, enc.length);

  const view = new DataView(memory.buffer);
  const len = view.getUint32(resPtr, true);
  const body = new TextDecoder().decode(
    new Uint8Array(memory.buffer, resPtr + 4, len),
  );
  witchy_free(resPtr, 4 + len);

  const nl = body.indexOf("\n");
  const tag = nl === -1 ? body : body.slice(0, nl);
  const rest = nl === -1 ? "" : body.slice(nl + 1);
  return { ok: tag === "ok", text: rest };
}

// Examples — the same shapes the language reference verifies, so the playground
// demonstrates exactly what the docs promise.
const EXAMPLES = {
  "Actors + generators + caps": `import iter

// A PURE generator — no capability parameters, so it provably can't print, read
// a file, or touch the network. It can only compute: a stream of sensor
// readings from a little linear-congruential walk.
gen fn readings(seed: Int) -> Iter(Int):
    var x = seed
    while true:
        x = (x * 33 + 1) % 97
        yield x

// An actor that DOES hold authority: the Console it was granted at spawn. It
// tracks the highest reading and reports each one. Only this actor can print,
// because only it was given a Console — the generator above never could.
actor Monitor:
    console: Console
    var high: Int = 0

    on Reading(value: Int):
        if value > high:
            high = value
        print(console, "reading " <> int_to_string(value) <> "  (high " <> int_to_string(high) <> ")")

fn main(console: Console):
    // Authority enters here, at main. We hand it to the actor at spawn.
    let monitor = spawn Monitor(console)
    // Take a finite prefix of the infinite generator, then feed the actor.
    let sample = iter.collect(iter.take(readings(1), 6))
    for r in sample:
        send(monitor, Reading(r))
`,
  "Primes": `import list
import string

// Trial division — small and clear.
fn is_prime(n: Int) -> Bool:
    if n < 2:
        false
    else:
        var d = 2
        var prime = true
        while d * d <= n:
            if n % d == 0:
                prime = false
            d = d + 1
        prime

fn show(xs: List(Int)) -> String:
    string.join(list.map(xs, fn(n: Int): int_to_string(n)), " ")

fn main(console: Console):
    let primes = [n for n in 2..50 if is_prime(n)]
    print(console, "primes under 50:")
    print(console, show(primes))
    print(console, "found " <> int_to_string(length(primes)))
`,
  "FizzBuzz": `// Matching on a tuple of two conditions — no if/else ladder.
fn fizzbuzz(n: Int) -> String:
    match (n % 3 == 0, n % 5 == 0):
        (true, true) -> "FizzBuzz"
        (true, false) -> "Fizz"
        (false, true) -> "Buzz"
        (false, false) -> int_to_string(n)

fn main(console: Console):
    for n in 1..21:
        print(console, fizzbuzz(n))
`,
  "Bar chart": `import string

type Bar:
    Bar(String, Int)

fn render(b: Bar) -> String:
    match b:
        Bar(label, n) -> string.repeat("#", n) <> " " <> label

fn main(console: Console):
    let data = [Bar("api", 12), Bar("web", 7), Bar("db", 18), Bar("cache", 3)]
    print(console, "requests/sec")
    for b in data:
        print(console, render(b))
`,
  "Actors": `// Two actors. The worker totals each list it's sent and hands the result to
// the reporter — they share nothing but messages.
actor Reporter:
    console: Console

    on Result(label: String, total: Int):
        print(console, label <> " total = " <> int_to_string(total))

actor Worker:
    reporter: Subject

    on Sum(label: String, xs: List(Int)):
        var total = 0
        for x in xs:
            total = total + x
        send(reporter, Result(label, total))

fn main(console: Console):
    let reporter = spawn Reporter(console)
    let worker = spawn Worker(reporter)
    send(worker, Sum("evens", [2, 4, 6, 8]))
    send(worker, Sum("odds", [1, 3, 5, 7, 9]))
`,
  "Pattern matching": `type Shape:
    Circle(Int)
    Square(Int)

fn area(s: Shape) -> Int:
    match s:
        Circle(r) -> 3 * r * r
        Square(w) -> w * w

fn main(console: Console):
    for s in [Circle(2), Square(3)]:
        print(console, "area: \${area(s)}")
`,
  "Comprehension": `import list
import string

fn show(xs: List(Int)) -> String:
    string.join(list.map(xs, fn(n: Int): int_to_string(n)), " ")

fn main(console: Console):
    print(console, show([n * n for n in 1..6]))
    print(console, show([n for n in 1..11 if n % 2 == 0]))
`,
  "Generators": `import iter
import list
import string

gen fn fibs() -> Iter(Int):
    var a = 0
    var b = 1
    while true:
        yield a
        let nxt = a + b
        a = b
        b = nxt

fn main(console: Console):
    let first8 = iter.collect(iter.take(fibs(), 8))
    print(console, string.join(list.map(first8, fn(n: Int): int_to_string(n)), " "))
`,
  "Result + ?": `import result

fn checked_div(a: Int, b: Int) -> Result(Int, String):
    if b == 0:
        Err("division by zero")
    else:
        Ok(a / b)

fn ratio(a: Int, b: Int, c: Int) -> Result(Int, String):
    let first = checked_div(a, b)?
    checked_div(first, c)

fn show(r: Result(Int, String)) -> String:
    match r:
        Ok(v) -> "ok: " <> int_to_string(v)
        Err(e) -> "err: " <> e

fn main(console: Console):
    print(console, show(ratio(100, 5, 2)))
    print(console, show(ratio(100, 0, 2)))
`,
  "Structural equality": `import option

fn main(console: Console):
    print(console, to_string([1, 2, 3] == [1, 2, 3]))
    print(console, to_string(Some("a") == Some("a")))
    let d = insert(insert(dict_new(), "k", 1), "j", 2)
    print(console, int_to_string(get_or(d, "j", 0)))
`,
  "Capabilities (a type error)": `// load only holds Dir[Read], so calling write is a COMPILE error —
// authority is checked at the type level. Try deleting the write line.
fn load(dir: Dir[Read], name: String) -> String:
    write(dir, "evil.txt", "nope")
    read(dir, name)

fn main(console: Console, dir: Dir[Read]):
    print(console, load(dir, "x"))
`,
};

// --- syntax highlighting (dependency-free; mirrors book/witchy-hljs.js) ------

const KEYWORDS = new Set(
  ("fn let var if else match for in while return break continue type trait " +
    "impl actor on import pub inout sink own move spawn where as gen yield")
    .split(" "),
);
const LITERALS = new Set(["true", "false"]);
const BUILTINS = new Set(
  ("print read write exists is_dir list subdir make_dir connect listen accept " +
    "send_line send_bytes recv_line recv_all recv_bytes close restrict now " +
    "get_env send length at push concat dict_new insert get_or has remove " +
    "update keys values pairs size to_string int_to_string string_length " +
    "char_count index_of split string_chars replace substring to_upper " +
    "to_lower trim starts_with ends_with contains int_to_float float_to_int " +
    "int_to_duration duration_to_int sqrt string_to_int fail")
    .split(" "),
);

function escapeHtml(s) {
  return s.replace(/[&<>]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;" })[c]);
}

// One pass: comments, strings (with ${...} interpolation), numbers (incl.
// duration suffixes), then words classified as keyword / literal / builtin /
// type (Title-case) / plain.
const TOKEN =
  /(\/\/[^\n]*|\/\*[\s\S]*?\*\/)|("(?:\\.|[^"\\])*")|(\b\d+(?:\.\d+)?(?:ms|hr|s|m|h|d|w)?\b)|([A-Za-z_][A-Za-z0-9_]*)/g;

function highlightString(str) {
  // Color ${...} interpolations distinctly inside an otherwise-green string.
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

function highlightWitchy(src) {
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
    else {
      const w = m[4];
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

function init() {
  const editor = document.getElementById("editor");
  const highlight = document.getElementById("editor-hl");
  const output = document.getElementById("output");
  const runBtn = document.getElementById("run");
  const picker = document.getElementById("examples");

  // Keep the highlighted layer in sync with the textarea's text and scroll.
  const sync = () => {
    // Trailing newline guard: a final \n in the textarea needs a matching line
    // in the <pre>, or the last visible line drifts.
    highlight.innerHTML = highlightWitchy(editor.value + "\n");
  };
  const syncScroll = () => {
    highlight.scrollTop = editor.scrollTop;
    highlight.scrollLeft = editor.scrollLeft;
  };
  editor.addEventListener("input", sync);
  editor.addEventListener("scroll", syncScroll);
  // A tab inserts four spaces instead of leaving the field.
  editor.addEventListener("keydown", (e) => {
    if (e.key === "Tab") {
      e.preventDefault();
      const s = editor.selectionStart;
      const eN = editor.selectionEnd;
      editor.value = editor.value.slice(0, s) + "    " + editor.value.slice(eN);
      editor.selectionStart = editor.selectionEnd = s + 4;
      sync();
    }
  });

  for (const name of Object.keys(EXAMPLES)) {
    const opt = document.createElement("option");
    opt.value = name;
    opt.textContent = name;
    picker.appendChild(opt);
  }
  const load = (name) => {
    editor.value = EXAMPLES[name];
    output.textContent = "";
    output.className = "";
    sync();
  };
  picker.addEventListener("change", () => load(picker.value));
  load("Actors + generators + caps");

  const run = () => {
    if (!wasm) {
      output.textContent = "still loading the interpreter…";
      return;
    }
    try {
      const { ok, text } = runWitchy(editor.value);
      output.textContent = text || (ok ? "(no output)" : "(empty error)");
      output.className = ok ? "ok" : "err";
    } catch (e) {
      output.textContent = "interpreter crashed: " + e.message;
      output.className = "err";
    }
  };
  runBtn.addEventListener("click", run);
  editor.addEventListener("keydown", (e) => {
    if ((e.metaKey || e.ctrlKey) && e.key === "Enter") {
      e.preventDefault();
      run();
    }
  });

  loadWitchy()
    .then(() => {
      runBtn.disabled = false;
      runBtn.textContent = "Run  (⌘/Ctrl+Enter)";
    })
    .catch((e) => {
      output.textContent =
        "failed to load witchy.wasm — did you run ./scripts/build-playground.sh and serve over HTTP?\n\n" +
        e.message;
      output.className = "err";
    });
}

document.addEventListener("DOMContentLoaded", init);
