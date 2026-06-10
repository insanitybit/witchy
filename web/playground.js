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
  "Hello": `fn main(console: Console):
    print(console, "hello, witchy")
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

function init() {
  const editor = document.getElementById("editor");
  const output = document.getElementById("output");
  const runBtn = document.getElementById("run");
  const picker = document.getElementById("examples");

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
  };
  picker.addEventListener("change", () => load(picker.value));
  load("Hello");

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
