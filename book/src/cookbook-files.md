# Reading and Writing Files

File access in witchy flows through the `Dir` capability. A `Dir` is authority
over a directory *subtree* — never the whole filesystem — and it carries rights
that say what you may do inside it. You read files with `Dir[Read]`, write them
with `Dir[Write]`, and take a full `Dir` when you need both. Because the
examples here perform real I/O, the book type-checks them but does not run them;
each is a complete program you could hand to `witchy run --dir . program.witchy`.

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

Run `witchy caps` on this and the footprint is exactly `Console, Dir[Read]` —
the signature is a machine-checkable promise that the program cannot write, only
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

`subtree` is the key to *attenuation*: `logs` is a `Dir` scoped to the `logs/`
folder, and a helper you pass it to can touch nothing outside. This is how you
hand a subsystem the narrowest slice of filesystem authority it needs — the same
principle as narrowing rights, applied to *reach* instead of *verb*.

## The `fs` helpers

`Dir` gives you the primitives; the `fs` module adds a few common compound
operations on top. `fs.ensure_dir(root, path)` creates a directory and any
missing parents, and `fs.collect_files(root, path, rel, ext)` walks a subtree and
returns the files matching an extension — the building block for a scanner or a
build step. `path` (covered in the appendix) handles the pure string side:
joining, normalizing, and splitting paths without touching the disk at all.

The division of labor is deliberate: `path` never needs a capability because it
only manipulates strings, `Dir` is the unforgeable authority to actually reach
the filesystem, and `fs` is convenience built from `Dir`. Keep path math in
`path`, take the narrowest `Dir` you can at the boundary, and the reach of any
file bug is bounded by the subtree you granted.
