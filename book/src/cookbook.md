# The Standard Library in Practice

The earlier chapters taught the language; this section is a working programmer's
tour of the library that ships with it. Each chapter takes one everyday job —
matching text, deduplicating a list, parsing a config file — and shows the
module built for it, with runnable examples you can lift straight into your own
code.

Two things are true of every example here, and worth stating once:

- **Every example is a real, tested program.** The book's ` ```witchy ` blocks
  are compiled, type-checked, and run on *both* backends as part of the build.
  If a snippet shows an output, that output is what the toolchain produced — it
  cannot drift out of sync with the language.
- **A module is one `import` away.** Eight modules form the prelude (`list`,
  `string`, `dict`, `math`, `option`, `result`, `policy`, `show`) and need no
  import. Everything else — `regex`, `set`, `url`, `encoding`, `func`, `toml`,
  and the rest — is brought in with a single `import name` line, and its
  functions are called module-qualified (`regex.matches(...)`).

For the exhaustive, function-by-function signatures, see
[Appendix: The Standard Library](appendix-stdlib.md) and the generated
[spec/stdlib.md](https://github.com/insanitybit/witchy/blob/master/spec/stdlib.md).
When you know roughly what you want but not the name, `witchy which <fragment>`
searches the whole library from the command line.
