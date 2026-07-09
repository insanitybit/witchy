// The witchy playground: the real compiler (`web/witchy.wasm`) compiling + running snippets
// ENTIRELY in the browser via the shared host shim (`web/witchy-host.js`) — the SAME engine the
// docs cells and `scripts/pg_validate.mjs` use, so it is oracle-validated. Syntax coloring is
// the shared `web/witchy-highlight.js`. No server: the compiler runs in your browser.
import { runWitchy } from "./witchy-host.js";
import { highlightWitchy } from "./witchy-highlight.js";

// The example programs — each a complete, CURRENT witchy program (compile-checked by
// `playground-examples.test.mjs`). "Capabilities" is INTENTIONALLY a compile error: that is the
// point — authority is enforced at the type level, so `write` on a `Dir[Read]` won't compile.
export const EXAMPLES = {
  "Pattern matching": `type Shape:
    Circle(Int)
    Square(Int)

fn area(s: Shape) -> Int:
    match s:
        Circle(r) -> 3 * r * r
        Square(w) -> w * w

fn main(console: Console):
    for s in [Circle(2), Square(3)]:
        console.print("area: \${area(s)}")
`,
  "FizzBuzz": `// Matching on a tuple of two conditions — no if/else ladder.
fn fizzbuzz(n: Int) -> String:
    match (n % 3 == 0, n % 5 == 0):
        (true, true) -> "FizzBuzz"
        (true, false) -> "Fizz"
        (false, true) -> "Buzz"
        (false, false) -> "\${n}"

fn main(console: Console):
    for n in 1..21:
        console.print(fizzbuzz(n))
`,
  "Comprehensions": `import list

fn main(console: Console):
    let squares = [n * n for n in 1..6]
    console.print(list.join(list.map(squares, fn(n: Int): "\${n}"), " "))
    let evens = [n for n in 1..11 if n % 2 == 0]
    console.print(list.join(list.map(evens, fn(n: Int): "\${n}"), " "))
`,
  "Generators": `import iter
import list

// A PURE generator — no capability parameters, so it provably can't print, read a file, or
// touch the network. It can only compute: an infinite stream, taken finitely.
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
    console.print(list.join(list.map(first8, fn(n: Int): "\${n}"), " "))
`,
  "Errors as values": `import result

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
        Ok(v) -> "ok: \${v}"
        Err(e) -> "err: \${e}"

fn main(console: Console):
    console.print(show(ratio(100, 5, 2)))
    console.print(show(ratio(100, 0, 2)))
`,
  "Capabilities (a type error)": `// \`load\` only holds Dir[Read], so calling write is a COMPILE error — authority is checked
// at the type level. Delete the write line and it compiles.
fn load(dir: Dir[Read], name: String) -> String:
    dir.write("evil.txt", "nope")
    dir.read(name)

fn main(console: Console, dir: Dir[Read]):
    console.print(load(dir, "x"))
`,
};

let wasm = null;
async function loadCompiler() {
  const resp = await fetch("witchy.wasm");
  const bytes = await resp.arrayBuffer();
  const { instance } = await WebAssembly.instantiate(bytes, {});
  wasm = instance.exports;
}

function init() {
  const editor = document.getElementById("editor");
  const highlight = document.getElementById("editor-hl");
  const output = document.getElementById("output");
  const runBtn = document.getElementById("run");
  const picker = document.getElementById("examples");

  // Keep the highlighted layer in sync with the textarea's text + scroll. innerHTML here is
  // the ONE HTML sink, over the TRUSTED highlighter's output (the user's code, HTML-escaped).
  const sync = () => {
    highlight.innerHTML = highlightWitchy(editor.value + "\n");
  };
  const syncScroll = () => {
    highlight.scrollTop = editor.scrollTop;
    highlight.scrollLeft = editor.scrollLeft;
  };
  editor.addEventListener("input", sync);
  editor.addEventListener("scroll", syncScroll);
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
  load(Object.keys(EXAMPLES)[0]);

  const run = async () => {
    if (!wasm) {
      output.textContent = "still loading the compiler…";
      return;
    }
    runBtn.textContent = "running…";
    try {
      const { ok, text } = await runWitchy(wasm, editor.value);
      output.textContent = text || (ok ? "(no output)" : "(empty error)");
      output.className = ok ? "ok" : "err";
    } catch (e) {
      output.textContent = "playground error: " + ((e && e.message) || e);
      output.className = "err";
    }
    runBtn.textContent = "Run  (⌘/Ctrl+Enter)";
  };
  runBtn.addEventListener("click", run);
  editor.addEventListener("keydown", (e) => {
    if ((e.metaKey || e.ctrlKey) && e.key === "Enter") {
      e.preventDefault();
      run();
    }
  });

  loadCompiler()
    .then(() => {
      runBtn.disabled = false;
      runBtn.textContent = "Run  (⌘/Ctrl+Enter)";
    })
    .catch((e) => {
      output.textContent =
        "failed to load witchy.wasm — run ./scripts/build-playground.sh and serve over HTTP\n\n" +
        ((e && e.message) || e);
      output.className = "err";
    });
}

if (typeof document !== "undefined") {
  document.addEventListener("DOMContentLoaded", init);
}
