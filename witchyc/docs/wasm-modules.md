# Modules

Each file is a module. `import "path.wy"` brings another file's declarations
into the program; each file is included once.

## The standard library by name

`import console`, `import deque`, `import hashmap` and so on name the standard
modules, which the compiler carries inside itself, so a program anywhere can
use them without a path (book: tour-modules.md, "bundled standard-library names
are reserved and always resolve to the compiler's shipped modules"). The names
are the library files' without `wasm-` and `.wy`, with `_` for `-`
(`json_tree`). A standard module's own imports are standard modules too, and an
unknown name is refused with the list of names. The shipped copy is the
library as it was when the compiler was built. Mixing `import console` with a
path import of the same library file includes it twice and is refused as a
duplicate declaration.

## Privacy

Functions and methods are private to their file unless declared `pub`:

```
pub fn area(shape: &Box) -> U64:
    return product(shape.width, shape.height)

fn product(a: U64, b: U64) -> U64:   // only this file may call it
    return a * b

impl Box:
    pub fn perimeter(shape: &Box) -> U64: ...
```

- A call to a private function or method from another file is refused, naming it.
- The methods of a trait impl are as visible as the trait; trait declarations are
  public.
- The language's own `Text` methods are public.
- Types and traits are private the same way. Another file that
  names one (in a signature, a constructor, a pattern, a bound or `Type.method`)
  is refused at that name. A private sum's variant names are not checked, since
  anonymous unions elsewhere may use the same tags; without the type's name a
  program can still match on such a value it is given. A name after `.` is a field or method and is not
  checked as a type. Sealing still governs who may build or open a public type.
- `pub let` offers a module constant to the files that import its file; a
  constant is otherwise its own file's (see below).

The library marks its interface `pub`. Free functions used only inside their
own file stay private, among them the unchecked-memory helpers
(`memory_take_prefix`, `memory_move_prefix`, `memory_take_wrapped`), the UTF-8
helpers and the JSON parser's internals.

Fixtures: `tests/fixtures/modules/`.

## What a file sees (2026-11-22)

A file sees its own declarations, what the files it imports itself offer with `pub`, and
what the compiler declares for every file (the built-in types, `Text` and its methods, the
call traits). Nothing is in reach because another file imports it, because it is the only
declaration of its spelling, or because it is someone else's package's. This holds for every
kind of name: functions, types and traits (in annotations, parameters, results, bounds, type
arguments, `impl Trait for Type`, constructors and `Type.function`), variant constructors,
constants (`pub let`) and derive recipes (named qualified, through an import).

Methods and impls are found by a value's type, not by a name in scope, so they have one more
way in. A method, or an impl a call or a bound uses, is in reach when its file is (by the rule
above), or when it is written in the type's own file or the trait's own file and that file is
of the using file's own package or of the standard library. A built-in type (`U64`, `Result`,
`Console`, `Text`) has no file: its methods and impls are found in those files. An operator
(`a + b`, `a < b`) names no trait: it uses the impl for the operands' type, found the same way
(`>` and `<=` through the standard library's `sort_after`, whatever else a program declares by
that name). A blanket impl's method (`impl<T> Trait for T`) is found through its trait the same
way. A pattern's variant names (`Yes:`, `No(n):`) are read against the matched value's own type,
as `.Name` is, and are not checked as names in reach: a value a file was handed can be matched.
Inside a `${...}` value `@`, `#` and `__witchy_` are not written, at any depth (they spell the
compiler's own names).

A package is the files joined by relative imports: the program's own files are one, the
standard library's are one, and each package imported by name (`import "@alpha"`) starts its
own with its first file. Another package's first file is in reach only where it is imported;
its other files never are, nor the methods and impls they declare. So a package that imports
nothing calls nothing of another package's, whatever the other marks `pub`, and the lock's
`requires` edges are the build's edges.

Within one package and in the standard library, a file names what it uses: `String` needs
`import string` (or the string file), though `console` imports it too; `Vec`'s `discard`
(written in `discard`, not beside `Vec` or `Discard`) needs `import discard`. A refusal says
which file declares the name: "`alpha_internal` is declared in a file this one does not import
(inner.wy:1)". A cycle of imports is refused (its edge would point at a file not yet read).

Fixtures: `tests/fixtures/packages/reach-*.wy` (each leak form, refused, and what a program may
use), `tests/fixtures/modules/reach-*.wy` (one package's files).

## One of each name, per file

Within one file a top-level name is declared once; a second declaration is refused where
it is written (`tests/fixtures/modules/same-name.refused.wy`). Across files, each file's
functions and types are its own (types: "Types are per file" below).

Each file's functions are its own. Two files, or a file and the program,
may each have a `helper`, public or not; an unqualified call means the calling
file's own, and another file's private one stays unreachable ("`helper` is
private to the module that declares it"). Inside the compiler such a function is
`helper@file`, and a call looks for its own file's first, then for the one name
every file shares, then for the only public one of a file it imports; names no two files share keep
their spelling (`modules/same-private.wy`, `modules/other-private.refused.wy`).

## Qualified calls

As in the book, a call may name the module: `other.area(5)` is the function
`area` of the file this one imports as `other`. A module's name is its file's
stem (`parts/other.wy` is `other`), and a standard module's is the name it is
imported by (`import console` is `console`); a file whose stem is not a plain
lowercase name is imported but cannot be named. When two imported files both
offer `area`, the unqualified call is refused ("`area` is offered by more than
one imported module: name the one meant, as `module.area`"), so neither import
silently wins (`modules/qualified.wy`, `modules/same-name.refused.wy`).

- Only a file that imports a module names it; one imported by another file is
  refused with that reason (`modules/qualified-unimported.refused.wy`).
- Naming the module reaches only what it offers with `pub`
  (`qualified-private.refused.wy`), and a name it lacks is refused as such
  (`qualified-missing.refused.wy`).
- A local of the module's name comes first: `console.print_int(x)` with a
  `console` parameter is a method call on the capability, as before.

Types and constructors may be named the same way: `area.Box(3, 4)`,
`let s: shapes.Shape = ...`, `vec.Vec<U64>`, `vec.Vec.new(...)`, and a match
arm `shapes.Strip(length):`. `from shapes import Shape, Square` checks, at its
line, that the module offers each name (`modules/qualified-types.wy`,
`standard-qualified.wy`, `qualified-type-private.refused.wy`,
`from-missing.refused.wy`). Qualification
says which file's type is meant; the module named must be imported here and must offer the
type with `pub`. Opening a record with
`let Name(a, b) = ...` takes the bare name.

Bundling writes each import line as `import NAME FILE` on the same line, so the
parser learns which file a name means while every file keeps its line numbers;
`from` lines stay in the header.

## Constants

A module constant (`let LIMIT = 10`) is its file's: another file that uses it,
in code or in its own constant, is refused ("this module constant is private to
the file that declares it; that file offers it with `pub let`"). `pub let LIMIT
= 10` offers it (`modules/constants.wy`, `private-constant*.refused.wy`).
A constant's name is its file's, as types are: in a file that neither declares it nor
imports a file offering it with `pub let`, the name means whatever that file says
(`modules/constant-named-like-library.wy`: a program's `K`, `count` and `index` beside
the library's parameters of those names, which once broke the library). In a file where it
does mean the constant, declaring something else with the name is refused. Two imported
files offering one name is refused at the use; name the one meant, `small_limits.LIMIT`,
in code or in another constant's value (`modules/qualified-constant.wy`).

## Not yet

Following the book (tour-modules.md), still to come: a prelude of always-available modules (`import console` is the one
line most programs need).

## Types are per file

Two files may each declare a type (or trait, capability or generator) of the same name,
private or `pub`: before parsing, a name declared by several files becomes the declaring
file's own (`src/parser/private_types.rs`), so each file's `Entry` is a different type with
its own fields and methods (`modules/private-types.wy`, `modules/private-generators.wy`).
Elsewhere a shared `pub` name is chosen like this (`modules/public-types.wy`):

- `orders.Item` is the one of the file imported as `orders`;
- after `from stock import Item`, a bare `Item` in this file is `stock`'s;
- a bare `Item` in a file that imports exactly one module offering it is that module's, so
  a library's own uses never change meaning when a program adds a type of the same name
  (`modules/own-entry-beside-hashmap.wy`: a program's `Entry` beside `HashMap`'s);
- with two such imports a bare `Item` is refused, naming the choices
  (`public-types-ambiguous.refused.wy`), and the two types never mix
  (`public-types-mixed.refused.wy`).
