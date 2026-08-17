---
rfc: 0123
title: "Newline-terminated expressions and `;` as discard"
status: proposed
created: 2026-08-17
predecessors:
  - "[0043](0043-declared-mutation-writeback.md) (`let _ =` as the explicit discard; non-`var` non-`Nil` throwaway is an error)"
  - "[0122](0122-uniform-borrow-relations.md) (`&place`, `&mut place`, and `*place` reuse infix glyphs)"
related:
  - "[0064](0064-complete-mutation-classification.md) (statement-position write-back and the discard diagnostic)"
  - "[0078](0078-anonymous-tagged-unions.md) (inline-arm `.Tag` is the next pattern, not a method)"
tracking: "unimplemented; staged in three cuts"
---

# RFC-0123: Newline-terminated expressions and `;` as discard

> Provisional syntax. Code blocks are deliberately **not** tagged `witchy`,
> so the doc-examples sweep does not compile pre-implementation snippets.

## Summary

The layout pass already knows where a statement starts. It does not emit a
separator there. This RFC makes it emit a virtual `Tok::StmtSep` at each
same-indent line boundary inside a statement or arm-list block, *unless*
the next line's first token cannot start a statement (`.`, `?`, `as`, and
the infix-only glyphs). Dual glyphs (`*`, `&`, `-`) get the separator
from indent alone.

An author-written `;` is a different token, `Tok::Semi`. Both stop
`expr()`. Only `Semi` builds `Stmt::Discard`. That mark *replaces*
`let _ = e`. `let`, `var`, assignment, `for`, `return`, and the other
non-value forms do not take a semicolon.

## Motivation

`apply_layout()` (`lexer.rs:1199`) already groups tokens by line, records
`bdepth_start` (bracket depth at the first token), and manufactures virtual
`LBrace`/`RBrace` from `:`/`->` headers. Newlines are not trivia. The
defect is narrower: **inside a block, two statements at the same indent
have no separator**, so the Pratt parser cannot tell them apart.

RFC-0122 made `*` prefix dereference and left it as multiply. Without a
separator,

```text
let slot = select(&mut pair, true)
*slot = 9
```

is `select(...) * slot = 9`. `expr()` stops climbing when the next token
is on a new line *and* `is_assignment()` says the line is a place write.
Match arms have a sibling rule for newline `-`. `&` is the same shape and
has no such rule. `let view = &text` on one line and `&other` on the next
is a live misparse in `mode opt`. Every dual glyph we add becomes another
`if` in `expr()`.

A glyph table in `expr()` would reconstruct a boundary layout already
computed as indent plus `bdepth_start`. Pure indent, with a separator at
every same-indent line, would break the 26 same-indent method chains in
the corpus, including this RFC's own showcase
(`projects/coven/src/coven_proto.witchy:68`, `.and_then` at indent 8
after a receiver at indent 8). The synthesis is: indent decides the dual
glyphs; a closed deny-list of tokens that *cannot* start a statement
suppresses the separator so those 26 sites stay.

Requiring `;` after every statement would also stop the climb. It would
tax every normal-mode file, layer a second statement system on the
off-side rule, and force a one-cut rewrite of `std/`, the book, and every
executed spec fence.

There is a second, real awkwardness that an author-written `;` *does*
earn. A last-line expression is the block value. A mid-block non-`var`,
non-`Nil` call is an error unless the author writes `let _ =`. So the
only way to run something for its effect and still have the block be
`Nil` is a dummy binding:

```text
fn setup(xs: var List(Int)) -> Nil:
    xs.push(1)
    let _ = xs.length()
```

`push` is a `var` mutator and returns `Nil`, so a last-line push is
already fine. `length` is not. `let _ =` works. It is also a lie: nothing
is being bound. The spec says `let _ = e` and a bare expression statement
mean the same thing, and `fmt` prints the bare form. We already have a
discard. We spelled it like a binding.

## Design

### 1. Layout emits `StmtSep` at same-indent statement boundaries

Phase 2 of `apply_layout()` already identifies the line that starts a new
statement: inside a virtual block, a line whose indent equals the body
indent and whose `bdepth_start` equals the block's depth. It already
emits `LBrace` on the first such line. Before each subsequent one it
emits a virtual `Tok::StmtSep`, **unless** that line's first token is in
the deny-list below.

`vtok` is `Token::point(...)` with no virtual marker (`lexer.rs:1182`).
A layout-emitted token and a typed `;` would be indistinguishable if they
shared a kind. They must not. Layout emits `Tok::StmtSep`. The author
writes `Tok::Semi`. Neither is in `infix_bp`, so both stop `expr()`.
`stmt()` accepts either as a terminator. Only `Tok::Semi` produces
`Stmt::Discard`. Then `xs.length()` / newline / `xs.push(1)` is
`xs.length() StmtSep xs.push(1)`, a mid-block expression statement, and
the RFC-0043 must-bind error still fires. `xs.length();` is
`xs.length() Semi`, a discard.

`bdepth_start` is the depth gate. Phase 1 already tracks `LParen |
LBracket | LBrace | QuoteHoleStart | DotLBrace | DotLBracket`
(`lexer.rs:1230-1236`). `DotLBrace` is `.{ x: 1, y: 2 }`; `DotLBracket`
is the anonymous-union type. A multi-line `.{ … }` does not get a
`StmtSep` in the middle: those lines start at a deeper `bdepth_start`.

#### Deny-list: tokens that cannot start a statement

If the next line's first token is one of these, suppress `StmtSep`:

`.` `?` `as` `+` `/` `%` `==` `!=` `<` `>` `<=` `>=` `&&` `||` `??`
`|` `^` `<<` `>>` `..` `..=`

That is a closed list of postfix-only and infix-only tokens. It lives in
`apply_layout()`, next to the line structure it already owns. It is not
an `if` in `expr()`.

The dual glyphs `*` `&` `-` are **not** on the list. Same-indent `*slot = 9`
and `&other` get a `StmtSep`. That is the RFC-0122 win, and it does not
depend on classifying those glyphs as prefix. The list only grows when
a *new postfix or infix-only* glyph appears, which is the benign
direction.

The 26 same-indent leading-dot chains in `std/`, `projects/`, and
`examples/` (coven.witchy:240-255, coven_proto.witchy:68, oauth.witchy,
two glamour examples, serve_hello) keep working. No `fmt` re-indent.

#### Which blocks get a separator

`StmtSep` is emitted inside **statement blocks** and **arm-list blocks**.

Statement blocks are bodies opened by `fn` / `if` / `else` / `for` /
`while` / `comptime` / `region` and by `->` (match-arm bodies). The
header classifier skips `pub` / `async` / `gen` to find the head
keyword, so `pub fn`, `async fn`, `gen fn`, and `pub async fn` still
get separators. A block-bodied lambda (`list.map(xs, fn(c):` plus an
indented body) is a `fn` header and gets them. A default method's `fn`
body inside a `trait` is included even though the `trait` body itself
is not.

Arm-list blocks are opened by `match`. They get `StmtSep` between arms.
`match_expr` already eats an optional `Comma` at `parser.rs:3983`; it
eats `Comma` or `StmtSep`. Then `.Err` at arm indent is the next arm,
and the `postfix()` uppercase-dot branch (`1847-1860`) comes out.

`StmtSep` is *not* emitted inside `type` / `trait` / `impl` bodies.
Those are lists of fields or methods, not statements or arms.

There is no `actor` form. The lexer has no `Tok::Actor`.

`let x = match y:` opens an arm list, correctly.

#### What falls out

```text
let slot = select(&mut pair, true)
*slot = 9
```

Same indent, `*` is not on the deny-list: `StmtSep`, two statements.

```text
let files = json.get(doc, "source")
    .and_then(fn(src: Json): json.get(src, "files"))
    .and_then(fn(a: Json): json.as_array(a))
```

This is the real shape at `coven_proto.witchy:64-68` (same indent).
`.` is on the deny-list: no `StmtSep`, one expression. A more-indented
`.and_then` also continues, because it is not a same-indent boundary.

```text
dir
    as Dir[Read]
```

and same-indent `as` both continue: `as` is on the deny-list.

```text
let total = a
    + b
    - c
```

`+` is on the deny-list, so `+ b` continues. `-` is dual and is *not*
on the list. Same-indent `- c` after `a + b` gets a `StmtSep` and is a
new statement; the parser rejects it (see §5). Indented `- c` is not a
same-indent boundary, so it continues as subtract and the sum is
`a + b - c`.

```text
match x:
    .Ok(v) -> v
    .Err(e) -> e
```

Arm-list `StmtSep` between the two lines. `match_expr` eats it. The
inline body `.Ok(v) -> v` ends with `v`, not `->`, so phase 2 never
opened a block around `v`. The arm-list separator is what makes
`.Err` the next arm. That is why the separator must be emitted in
arm-list blocks, and why "a bug in the emit rule" was the wrong
diagnosis.

#### Guards that stay

These are not continuation rules. They stay.

- `is_assignment()` itself. Statement dispatch at `parser.rs:1643` and
  inline-arm-body dispatch at `3969` keep it. Only its newline use in
  `expr()` goes.
- Same-line `[` / `(` in `postfix()` (`1818`, `1829`) and `.Tag(`
  payloads (`3641`). No bracket-depth gate. Folding them into a
  depth-gated rule would rejoin `a\n[0]` inside parentheses.
- `?` message operand on the same line (`1802`).
- `name_application()` (`3690`): a `(` that begins a new line is never
  call arguments. An interpolated string expands to a leading `(`, so
  `else: x` followed by `"${…}"` would otherwise become `x(...)`. That
  is the form people write. Six newline-sensitive sites, not five:
  the five `on_same_line_as_prev()` calls plus this raw line compare.

### 2. Author `;` discards an expression

The author writes `Tok::Semi`. Layout never emits it. Today `;` is a
lex error. Strings and comments keep `;` as text. A `;` in
expression-interior position (`f(a; b)`) is a parse error, not a
discard.

An expression statement that ends in a written `;` means: evaluate,
discard, do not use as the block value.

```text
fn setup(xs: var List(Int)) -> Nil:
    xs.push(1)
    xs.length();
```

Last-line `xs.length()` is the block value, an `Int`. Last-line
`xs.length();` is a discard and the block is `Nil`. Mid-block, a
non-`var`, non-`Nil` call without a written `;` is still the RFC-0043
error, because the layout separator is `StmtSep`, not `Semi`.
Mid-block `e;` is the explicit discard that used to be `let _ = e`.

Represent this as **`Stmt::Discard(Expr)`**, a new variant, not a field
on `Stmt::Expr`. `Stmt::Expr(Expr)` has hundreds of construction sites
(`parser_tests.rs` alone has 36). A field breaks all of them. A new
variant is additive: exhaustive matches break loudly at the sites that
must decide.

`;` is legal only on an expression statement. It is a parse error after
`let`, `var`, assignment, `for`, `while`, `return`, `break`, `continue`,
and `yield`. Those forms are already not values. The error is "`;`
discards an expression; this form is already not a value."

It is also illegal in expression-position bodies. An inline match arm
parses via `self.expr(0)` unless it starts with `return` / `break` /
`continue` / an assignment (`parser.rs:3966-3979`). So `0 -> log(x);`
is a parse error, as are `fn(x): e;` and `if c: a; else: b`. `;`
terminates a statement. Authors will try all three.

A block whose last form is `Stmt::Discard`, `let`, `var`, assignment, or
a looping statement has value `Nil`.

The RFC-0043 discard rule lives in
`crates/witchy-types/src/traits.rs:3314-3320` (`discarded_result_msg`),
in the mono/write-back rewrite, not in `typeck.rs`. That rewrite edits
the single AST both backends consume (`lower` / `lower_for_wasm`) and
the checker consumes (`lower_checked`). `;` inherits that if it is
`Stmt::Discard` on that AST. Parity holds by construction. Staging
step 2 edits the message to name `e;`.

### 3. `let _ = e` goes away

One cut. Reject `Stmt::LetPattern` whose `pattern` is exactly
`Pattern::Wildcard` (`ast.rs:968`). Nested wildcards are
`Pattern::Ctor` / `Pattern::Tuple` and stay: `let [first, ..rest] = xs`,
`let Point(_, y) = p`, `let (_, y) = pair`.

The four live `.witchy` sites (one in `projects/coven`, three in
`projects/glamour`) become `e;`. They sit under `projects/**/src/*.witchy`,
which is already a `witchy fmt` gate path, so the gate enforces the
migration. Spec, book, and RFC prose that teach `let _ =` as discard
move with the implementation. `book/examples.json` is regenerated with
the book fences. `spec/stdlib.md` has zero `let _ =` in `std/`
doc-comments; do not hand-edit it.

The same spelling also lives in witchy source embedded in Rust test
strings, where `fmt` cannot reach: `analysis.rs`, `async_lower.rs`,
`src/example_tests/*`, `diagnostic_golden_tests.rs`, `loans_tests.rs`,
`tests/typeck.rs`, `lsp_tests.rs`. Staging step 2 sweeps those fixtures
and regenerates goldens. A leftover `let _ = e` in a `.witchy` file is
a parse or check error that points at `e;`.

### 4. `fmt`

`fmt` stays a syntactic pass. It does not consult the type checker, and
it has to run on a file that does not typecheck.

- Rewrite `let _ = e` to `e;` in `.witchy` files. That migrates the four
  live sites. Embedded fixtures are a compiler-source edit, not a `fmt`
  job.
- Do not re-indent the 26 same-indent method chains. The deny-list is
  what keeps them legal.
- Preserve the author's `;`. Do not insert one to make a last-line
  expression match a `Nil` return. Layout's `StmtSep` is not printed.
- Never put `;` on `let`, `var`, assignment, `for`, `while`, `return`,
  `break`, `continue`, or `yield`.
- Do not sprinkle `;` on mid-block `Nil` or `var` calls. Those are
  already legal as bare expressions.

### 5. Diagnostics

The parser emits hard errors. A warning would leave the silent-`-c` hole
half-open.

- A `StmtSep` ended a complete expression and the next statement is a
  bare prefix-operator expression (`-`, `*`, `!`, `~`) that is neither
  bound nor assigned: name the glyph, say a same-indent line starts a
  new statement, and offer to indent the continuation or parenthesize.
  This is same-indent `- c` after `let total = a + b`.
  `fn ne(...) -> Bool:` / newline / `!eq(...)` and `_ ->` / newline /
  `-1` are not this error: `:` and `->` open a block, they do not end an
  expression.
- `;` after a non-expression form, including inline `0 -> e;`,
  `fn(x): e;`, and `if c: a;`: the sentence in §2.
- `;` inside an argument list or other expression-interior position: a
  parse error, not a discard.
- Discard of a non-`var`, non-`Nil` call: `traits.rs:3314-3320` points
  at `e;`, not `let _ = e`.

### 6. Editor grammar

Tree-sitter does not run `apply_layout()`. It already has an indent
scanner. That scanner emits a statement-break at the same indent inside
statement and arm-list blocks, suppressed when the next line starts with
a deny-list token, matching `StmtSep`. Then `*slot = 9` on the next line
is a statement without a hack that breaks `n * 2`, and
`coven_proto.witchy:68` still highlights as one expression.

## Alternatives

**Pure indent, separator on every same-indent line.** Token-agnostic.
Breaks 26 same-indent method chains in six gated files, including
`coven_proto.witchy:68`. Requires a `fmt` re-indent this RFC does not
want to own. Rejected after measuring the corpus.

**A continuing-token table in `expr()`.** Handles the 26 sites (column
is not part of the decision). Must grow for each new *dual* glyph, which
is the expensive direction. Reconstructs indent plus `bdepth_start` in
the wrong pass. Rejected.

**Require `;` on every statement.** Stops the climb. Charges the whole
language and fights the off-side rule. Rejected.

**Keep the lookahead and add a case for `&`.** Cheap this week. The next
dual glyph gets the same `if`. Rejected as policy.

**Give dereference and borrow new glyphs** (`x.*`, `ref x`). Taste, if
`&` / `*` stay confusing in `mode opt`. Not load-bearing once a
same-indent dual glyph is a new statement.

**Optional `;` and keep `let _ =`.** Two explicit discards. One-cut:
`;` wins.

**One token for layout and the author.** `vtok` has no virtual marker.
`xs.length()` / `xs.push(1)` would lex as an explicit discard and the
RFC-0043 error would never fire mid-block. Rejected: two kinds.

**Do nothing.** The `*` assignment lookahead stays, `&` stays wrong on a
newline, and last-line discard stays a fake binding.

**Go/JS automatic semicolon insertion.** Those guess from token
adjacency at end-of-line. This inserts from indentation, in the pass
that already synthesizes `{` and `}` from it, and then *withholds* the
separator when the next token cannot start a statement.

## Drawbacks

The deny-list is still a classification. It is small, closed, and grows
only for postfix or infix-only glyphs. The dual glyphs that motivated
the RFC are not on it.

Layout must classify statement-block and arm-list headers. The
classifier skips `pub` / `async` / `gen`. Getting `match` vs `->` wrong
either drops `StmtSep` between arms (and the `.Tag` branch has to stay)
or injects it inside a type body.

People will want `;` after `let`, and after `0 -> e`. The parse error
has to be early and dull.

Deleting `let _ =` is a one-cut the book, four project files, embedded
Rust fixtures, and `book/examples.json` have to take with the compiler.

## Prior art

Python's implicit line joining is the indent half: a more-indented line
continues, brackets join regardless. Same-indent `.method` is the extra
witchy already writes; the deny-list is how that extra is named.

Rust's optional semicolon is the discard half: `e` as the last form of
a block is the value; `e;` is `()`. This RFC takes that and does *not*
take "semicolon after every `let`."

RFC-0043 is the discard policy we are respelling. RFC-0122 is the
prefix reuse that made the missing separator loud.

## Staging

1. Lexer grows `Tok::StmtSep`. `apply_layout()` emits it in statement
   blocks and arm-list blocks, suppressed when the next line starts with
   a deny-list token. The header classifier skips `pub` / `async` /
   `gen`. `infix_bp(StmtSep)` and `infix_bp(Semi)` are both `None` once
   `Semi` exists; step 1 can treat `StmtSep` alone. The
   `is_assignment()` and match-arm `-` newline branches leave `expr()`.
   `is_assignment()` itself stays. `match_expr` eats optional `Comma` or
   `StmtSep`. The `postfix()` uppercase-dot branch is deleted. Same-line
   `[` / `(`, `.Tag(`, `?` operand, and `name_application()` `(` stay.
   The parser rejects a same-indent bare prefix statement. The 26
   same-indent leading-dot sites remain one expression.
2. Lexer accepts a written `;` as `Tok::Semi`. Parser builds
   `Stmt::Discard(Expr)` only for a written `;`. `LetPattern` whose
   pattern is exactly `Wildcard` is an error naming `e;`.
   `discarded_result_msg` in `traits.rs` names `e;`. `fmt` rewrites the
   four live `.witchy` sites (the `fmt` gate on
   `projects/**/src/*.witchy` keeps them rewritten) and the book/spec
   examples, then regenerates `book/examples.json`. A fixture sweep
   rewrites witchy source embedded in Rust tests (`analysis.rs`,
   `async_lower.rs`, `src/example_tests/*`, `diagnostic_golden_tests.rs`,
   `loans_tests.rs`, `tests/typeck.rs`, `lsp_tests.rs`) and regenerates
   goldens. Step 2 lands red without that sweep. Do not hand-edit
   `spec/stdlib.md`.
3. Tree-sitter's indent scanner emits the same statement-break, with the
   same deny-list.

Step 1 can land without step 2. Step 2 without step 1 still helps
last-line discard. The two tokens must not be collapsed.

## Acceptance

Parse shapes and parse errors are backend-independent. The one
parity-bearing row is the `;` block-value typing.

- `let slot = select(&mut pair, true)` / newline / `*slot = 9` at the
  same indent parses as two statements, with no `is_assignment()`
  newline special case.
- `let view = &text` / newline / `&other` at the same indent is two
  statements.
- `json.get(doc, "source")` / newline / same-indent
  `.and_then(fn(a: Json): json.as_array(a))` remains one expression
  (`coven_proto.witchy:64-68` and the other 25 same-indent leading-dot
  sites).
- `dir` / newline / same-indent `as Dir[Read]` remains one expression.
- An inline match of the form `.Ok(v) -> v` / newline / `.Err(e) -> e`
  still parses as two arms. `match_expr` ate a `StmtSep`. The
  `postfix()` uppercase-dot branch is gone. Lowercase same-indent
  `.method` after an arm body still chains (deny-list).
- `e` / newline / same-indent `? "msg"` is the message form of `?`.
  The operand stays on `?`'s line. `e?` / newline / `"msg"` is bare
  `e?` plus a string statement, unchanged from `parser.rs:1802`.
- `else: x` / newline / `"${y}"` at the body indent is `x` and then a
  string, not `x(...)`. `name_application()` at `parser.rs:3690` stays.
- `let total = a` / newline / indented `+ b` / newline / indented `- c`
  is one expression `a + b - c`. Same-indent `- c` after
  `let total = a + b` is a parse error, not a silent drop.
- Mid-block `xs.length()` / newline / `xs.push(1)` is still a discarded
  `length` under RFC-0043: the separator is `StmtSep`, not `Semi`.
- `fn ne(self: T, other: T) -> Bool:` / newline / `!eq(self, other)` is
  not an error, and neither is `_ ->` / newline / `-1`. The ten `ne`
  impls in `std/cmp.witchy` stay legal.
- `xs.length();` as the last form of a `-> Nil` function typechecks on
  both backends; the same call without `;` is the block's `Int`.
- `let x = 1;` is a parse error. `f(a; b)` is a parse error. `0 -> e;`,
  `fn(x): e;`, and `if c: a;` are parse errors. `";"` in a string and
  `;` in a comment are unchanged.
- `let _ = e` is an error that points at `e;`. The rejected node is
  `Stmt::LetPattern` whose pattern is exactly `Pattern::Wildcard`.
  `let (_, y) = pair` and `let Point(_, y) = p` stay. No discard
  `let _ =` remains under `std/`, `examples/`, `projects/`, `book/`,
  `spec/`, `book/examples.json`, or the Rust fixtures in Staging step 2.
  Rust `let _ =` in native harnesses is unaffected.
- `witchy fmt` rewrites `let _ = e` to `e;` and does not insert `;` on
  `let`, on a mid-block `var` call, or to satisfy a `Nil` return, and
  does not re-indent the 26 same-indent leading-dot sites.
- The tree-sitter parse of the RFC-0122 `*slot = 9` fixture has no
  `ERROR` node, `n * 2` still parses as multiply, and
  `coven_proto.witchy:68` is one expression.
