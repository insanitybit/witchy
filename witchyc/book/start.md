# Your first Witchy program

Save this as `hello.wy`.

```witchy
import console

fn main(console: Console<Write>):
    console.line("Hello, Witchy.")
```

`import console` brings the console library into scope. `fn main` declares the program's entry point, and the indented lines belong to its body. The parameter gives the program an output capability. Calling `console.line` uses that capability to write a line.

With the compiler and Wasmtime host available, build and run it.

```sh
witchyc build hello.wy -o hello.wasm
witchy-wasmtime-host hello.wasm --output
```

Try launching it again without `--output`. The same program requests output, but this time the launcher hasn't granted it. That difference is the starting point for the [capabilities chapter](capabilities.md).

The [installation and launch guide](../docs/wasm-production-build.md) explains how to set up the toolchain. The [runnable syntax tour](../tests/fixtures/programs/tour.wy) brings records, sums, functions, and control flow together once you're comfortable with this example.
