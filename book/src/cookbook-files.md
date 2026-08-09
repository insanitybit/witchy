# Reading and Writing Files

Open a file in most languages and you're spending your whole process's
filesystem authority. The path is just a string, and `../../../etc/passwd` is a
perfectly good string.

File access in witchy flows through the `Dir` capability instead. A `Dir` is
authority over a directory *subtree*, never the whole filesystem, and it carries
rights that say what you may do inside it. You read files with `Dir[Read]`, write them
with `Dir[Write]`, and take a full `Dir` when you need both.

Unlike the pure examples in earlier chapters, these programs need a real `Dir`
to run - so the book type-checks them, and in the online edition the **Run**
button executes them against a fresh *in-memory* `Dir`, one per run, seeded from
a fixed fixture (it contains a `config.toml`, among other files). Nothing touches
your actual disk. That same in-memory `Dir` is how you test file code
deterministically, shown at the end of this chapter. On your own machine you'd
grant a real subtree with `witchy run --dir . program.witchy`.

## Reading

A read-only file loader asks for `Dir[Read]` and nothing more. `exists` guards a
read, and `read` returns the file's contents as a `String`:

```witchy
fn main(console: Console, dir: Dir[Read]):
    if dir.exists("config.toml"):
        let text = dir.read("config.toml")
        console.print("read ${text.length()} bytes")
    else:
        console.print("no config found")
```

Run `witchy caps` on this and the footprint is exactly `Console, Dir[Read]` -
the signature is a machine-checkable promise that the program can't write, only
read, and only within the subtree it was granted.

## Writing and organizing

Writing needs the `Write` right. Some operations need more than one right at
once: `subtree` (which mints a `Dir` for a child directory) requires `Read`, so
a program that creates a folder *and* writes into it takes a full `Dir` rather
than a `Dir[Write]`:

```witchy
fn main(console: Console, dir: Dir):
    dir.make_dir("logs")
    let logs = dir.subtree("logs")
    logs.write("run.txt", "started\n")
    logs.append("run.txt", "finished\n")
    console.print("wrote logs/run.txt")
```

`subtree` is the key to *narrowing*: `logs` is a `Dir` scoped to the `logs/`
folder, and a helper you pass it to can touch nothing outside. This is how you
hand a subsystem the narrowest slice of filesystem authority it needs - the same
principle as narrowing rights, applied to *reach* instead of *verb*.

## The `fs` helpers

`Dir` gives you the primitives; the `fs` module adds a few common compound
operations on top. `fs.ensure_dir(root, path)` creates a directory and any
missing parents, and `fs.collect_files(root, path, rel, ext)` walks a subtree and
returns the files matching an extension - the building block for a scanner or a
build step. `path` (covered in the appendix) handles the pure string side:
joining, normalizing, and splitting paths without touching the disk at all.

The division of labor is deliberate: `path` never needs a capability because it
only manipulates strings, `Dir` is the unforgeable authority to actually reach
the filesystem, and `fs` is convenience built from `Dir`. Keep path math in
`path`, take the narrowest `Dir` you can at the boundary, and the reach of any
file bug is bounded by the subtree you granted.

## Testing file code with a virtual `Dir`

Because a `Dir` is just a value handed in at the boundary, you never need a real
filesystem to test file logic. The test runner mints an **in-memory `Dir`** from
a fixture plan and passes it to any `test_*` function that asks for one - the
exact mechanism the online book uses for its Run buttons. Reads see the fixture
files; writes and appends are visible to later reads within the run, then
discarded. The tests run identically on both backends:

```witchy
import testing

fn test_reads_a_config_file(dir: Dir[Read]):
    testing.assert_eq(dir.read("config.toml"), "mode = \"docs\"\n")

fn test_append_then_read(dir: Dir):
    dir.write("run.txt", "started\n")
    dir.append("run.txt", "finished\n")
    testing.assert_eq(dir.read("run.txt"), "started\nfinished\n")
```

You supply the starting contents in a small JSON fixture plan and run it with
`witchy test --fixtures plan.json --backend both file.witchy`. The plan for the
tests above declares `Read`/`Write` rights and one seed file:

```json
{
  "version": 1,
  "filesystem": {
    "rights": ["Read", "Write"],
    "entries": {
      "config.toml": { "kind": "file", "hex": "6d6f6465203d2022646f6373220a" }
    }
  }
}
```

File contents are given as `hex` so any bytes - text or binary - are expressible
(`6d6f6465203d2022646f6373220a` is `mode = "docs"\n`). This is the same fixture
model the [Testing](testing.md) chapter uses for `Clock`, `Env`, and the other
capabilities: authority comes from an explicit plan, so a unit test is
hermetic, deterministic, and never touches the real machine.
