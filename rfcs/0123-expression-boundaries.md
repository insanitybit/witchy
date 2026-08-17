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

Do not give witchy statement semicolons. End an unbracketed expression when a
newline is followed by a token that can start a statement, and keep climbing
only for tokens that can *only* continue an expression (`.`, `+`, `?`, and the
other infix-only or postfix-only glyphs). After an expression, `;` means
"evaluate this, discard it, it is not the block value" and *replaces*
`let _ = e`. `let`, `var`, assignment, `for`, `return`, and the other
non-value forms do not take a semicolon.

## Motivation

The compiler already special-cases a newline. RFC-0122 made `*` prefix
dereference and left it as multiply. The lexer treats a newline as trivia, so
the Pratt parser keeps climbing whenever the next token can be infix:

```text
let slot = select(&mut pair, true)
*slot = 9
```

A naive parse reads that as `select(...) * slot = 9`. `expr()` in
`crates/witchy-syntax/src/parser.rs` stops climbing when the next token is on a
new line *and* `is_assignment()` says the line is a place write. Match arms
have a sibling rule: a newline `-` is the next arm's negative pattern, not
subtraction. `on_same_line_as_prev()` has five call sites (the `?` message
operand, same-line `[` / `(`, inline-arm `.Tag`, and `.Tag(` payloads). Add
those two `expr()` branches and the newline rule is a pile of guards. This
RFC replaces the two `expr()` cases with one table. The same-line `[` / `(`
guards stay: they have no bracket-depth gate, and folding them into a
table that does not fire inside `()` would rejoin `a\n[0]` inside the
parens this RFC tells authors to write. The `.Tag` test is the leftover
that earns its own rule.

`&` is the same shape and has no such rule. `&` is bitwise-and *and* RFC-0122
borrow. `let view = &text` on one line and `&other` on the next is a live
misparse waiting to land in `mode opt`. Every dual glyph we add becomes
another `if` in `expr()`.

If the newline rule lands, the pressure for new `x.*` / `ref x` glyphs drops
to zero. Those spellings exist to dodge this clash. A table that stops `*`
and `&` at a newline makes the current glyphs safe.

Requiring `;` after every statement would stop the climb. It would also tax
every normal-mode file for an opt-mode token clash, layer a second statement
system on the off-side rule, and force a one-cut rewrite of `std/`, the book,
and every executed spec fence. The language already has statement structure:
a `:` opens an indented block, and a block's value is its last expression.

There is a second, real awkwardness that `;` *does* earn. A last-line
expression is the block value. A mid-block non-`var`, non-`Nil` call is an
error unless the author writes `let _ =`. So the only way to run something for
its effect and still have the block be `Nil` is a dummy binding:

```text
fn setup(xs: var List(Int)) -> Nil:
    xs.push(1)
    let _ = xs.length()
```

`push` is a `var` mutator and returns `Nil`, so a last-line push is already
fine. `length` is not. `let _ =` works. It is also a lie: nothing is being
bound. The spec even says `let _ = e` and a bare expression statement mean the
same thing, and `fmt` prints the bare form. We already have a discard. We
spelled it like a binding.

## Design

### 1. A newline ends an unbracketed expression unless the next token can only continue it

After a complete expression, if the next token is on a later source line and
is not inside `()`, `[]`, or `${...}`, classify it:

- **Continuing tokens** cannot start a statement. The parse keeps climbing.
  That set is the infix-only and postfix-only glyphs: `.` `+` `/` `%` `==`
  `!=` `<` `>` `<=` `>=` `&&` `||` `??` `|` `^` `<<` `>>` `?` `..` `..=`.
  Field access, method chains, and `e.await` keep working across lines:

  ```text
  let files = json.get(src, "files")
      .and_then(fn(a: Json): json.as_array(a))
  ```

- **Any other token** ends the expression. Identifiers, keywords, literals,
  `(`, `[`, and the dual glyphs (`*`, `&`, `-`) plus the other prefix operators
  (`!`, `~`, `move`) start a new statement. The RFC-0122 case becomes ordinary:

  ```text
  let slot = select(&mut pair, true)
  *slot = 9
  ```

  So does a match arm that begins with a negative literal, and a next-line
  borrow. `let x = a` followed by `* b` is `let x = a` and then a dereference
  of `b`. If the author meant multiply, the diagnostic says so and points at
  `a * b` on one line or `(a * b)`.

  `+` continues and `-` stops, so a wrapped sum splits:

  ```text
  let total = a
      + b
      - c
  ```

  That is `let total = a + b` and a second statement `-c`. A non-tail
  `-c` where `c: Int` typechecks and evaporates:
  `typeck.rs:6123-6137` only runs `reject_borrowed_nominal_runtime_ty` on
  non-tail expression statements, and the RFC-0043 must-bind rule is
  call-scoped. That is worse than `* b`, which usually fails as a deref.
  The parser rejects this shape as a hard error (§5). The author who meant
  a sum writes `(a + b - c)` or keeps the operators on one line.

Parentheses and brackets still join lines. This is the Python rule with one
deliberate extra: we continue across a newline when the next token *cannot*
be a statement. Strict Python would break every leading-dot chain. We have
those (see `projects/coven/src/coven_proto.witchy`).

`|` is bitwise-or in `infix_bp`. In pattern position `match_expr` consumes
`Bar` as an or-pattern separator before expressions run, so a line-leading
`|` inside a match is not this table's problem today. A later formatter that
wraps an or-pattern across lines has to keep that consumption order, or a
leading `|` would continue as bitwise-or.

`?` continues. It is postfix, not an `infix_bp` entry; the table is consulted
from both `expr()` and `postfix()`. The message form is `e? "msg"` with the
operand on `?`'s line. `postfix()` already gates that on
`on_same_line_as_prev()` (`parser.rs:1802`) and calls same-line consumption a
conservative extension. A `Str` on a later line is "any other token" and ends
the expression, so `e?` / newline / `"msg"` is bare `e?` plus a string
statement. That stays. The continuing form is `e` / newline / `? "msg"`.

`(` and `[` *end* the expression. That is pre-existing and unchanged:
`parser.rs:1818` and `1829` already require `[` / `(` on the same line as the
receiver, so `f` / newline / `(a, b)` is already a binding plus a tuple.
Call forms wrap the arguments: `f(\n    a, b\n)`.

The continuing-token set sits next to `infix_bp` and is read from `expr()`
and `postfix()`. `.` and `?` live only in the postfix half. Adding a new dual
glyph means adding it to the prefix set so a newline stops the expression.
Adding a new infix-only or postfix-only glyph means it continues. Compound
assignment (`+=` and friends) is not in this table. `infix_bp` has no arm for
it; `x += 1` on a new line already stops at the identifier `x`.

The off-side layout pass is unaffected. Indent still opens and closes
blocks. Column is not part of the decision: a more-indented continuing token
is still a continuation, and a more-indented `*` is still a new statement,
because `*` is dual. Authors who want a wrapped multiply write parens.

The two `expr()` newline branches (`is_assignment()` and the match-arm `-`
check) come out. The same-line `[` / `(` guards in `postfix()` stay. They
are pre-existing, they have no bracket-depth gate, and they must keep
stopping `a\n[0]` and `bar\n(1, 2)` *inside* parentheses. The table does
not fire inside `()`, `[]`, or `${...}`, so folding the guards into it
would change those forms from a parse error into an index or a call. The
`.Tag` test stays.

#### Residual: inline-arm `.Tag`

`postfix()` at `parser.rs:1847-1860` (RFC-0078): inside an inline match arm,
a next-line `.Tag` (dot, then an uppercase ident) is the *next anonymous-union
pattern*, not a method chain. Lowercase `.method` keeps continuing.

```text
match x:
    .Ok(v) -> v
    .Err(e) -> e
```

`.` cannot sit unconditionally in the continuing set *and* retire this
branch. Uppercase-vs-lowercase after `.` is per-construct lookahead, the
shape this RFC said it was deleting. It is not. The table retires the dual
glyphs and the match-arm `-`. The `.Tag` test stays, with an acceptance row
so a later cleanup cannot drop it by accident.

A follow-on can make next-line union variants take a leading `|`, or force
those arms into a block. That is a different RFC. This one does not pretend
the table ate it.

### 2. `;` discards an expression

`;` is a new lexer token (`Tok::Semi`). Today it is a lex error. Strings and
comments keep their current `;` as text. A `;` in expression-interior
position (`f(a; b)`) is a parse error, not a discard.

An expression statement may end in `;`. That mark means: evaluate the
expression, discard the result, and do not use it as the block value.

```text
fn setup(xs: var List(Int)) -> Nil:
    xs.push(1)
    xs.length();
```

Last-line `xs.length()` (no semicolon) is still the block value, an `Int`.
Last-line `xs.length();` is a discard and the block is `Nil`. Mid-block, a
non-`var`, non-`Nil` call without `;` is still the RFC-0043 error. Mid-block
`e;` is the explicit discard that used to be `let _ = e`.

`;` is legal only on an expression statement. It is a parse error after
`let`, `var`, assignment, `for`, `while`, `return`, `break`, `continue`, and
`yield`. Those forms are already not values. The error is "`;` discards an
expression; this form is already not a value."

A block whose last form is `e;`, `let`, `var`, assignment, or a looping
statement has value `Nil`. Same as a block that today ends on `let _ = e`.

### 3. `let _ = e` goes away

One cut. `let _ = e` is the discard spelling this RFC replaces. Pattern
bindings that use `_` in a real pattern stay (`let [first, ..rest] = xs`,
`let Point(_, y) = p`). Only the single-wildcard discard form is deleted.

The four live `.witchy` sites (one in `projects/coven`, three in
`projects/glamour`) become `e;`. Spec, book, and RFC prose that teach
`let _ =` as discard move with the implementation, not before. The same
spelling also lives in witchy source embedded in Rust test strings, where
`witchy fmt` cannot reach: `analysis.rs`, `async_lower.rs`,
`src/example_tests/*`, `diagnostic_golden_tests.rs`, `loans_tests.rs`,
`tests/typeck.rs`, `lsp_tests.rs`. Staging step 2 sweeps those fixtures and
regenerates goldens. A leftover `let _ = e` in a `.witchy` file is a parse
or check error that points at `e;`.

### 4. `fmt`

`fmt` stays a syntactic pass. It does not consult the type checker, and it
has to run on a file that does not typecheck.

- Rewrite `let _ = e` to `e;` in `.witchy` files. That migrates the four
  live sites. Embedded fixtures are a compiler-source edit, not a `fmt`
  job.
- Preserve the author's `;`. Do not insert one to make a last-line
  expression match a `Nil` return.
- Never put `;` on `let`, `var`, assignment, `for`, `while`, `return`,
  `break`, `continue`, or `yield`.
- Do not sprinkle `;` on mid-block `Nil` or `var` calls. Those are already
  legal as bare expressions.

### 5. Diagnostics

Two new messages, and one rewrite of an old one.

- The parser emits a hard error when a newline ended a complete expression
  and the next statement is a bare prefix-operator expression (`-`, `*`,
  `!`, `~`) that is neither bound nor assigned. Name the glyph, say it also
  starts a statement, and offer the one-line form or parentheses. This is
  the wrapped-sum case (`let total = a` / `+ b` / `- c`) as well as the
  multiply-as-deref case. A warning would leave the silent-`-c` hole
  half-open. The trigger is "the previous line ended a complete
  expression," so `fn ne(...) -> Bool:` / newline / `!eq(...)` and
  `_ ->` / newline / `-1` are not this error: `:` and `->` do not end an
  expression.
- `;` after a non-expression form: the sentence in §2.
- `;` inside an argument list or other expression-interior position: a
  parse error, not a discard.
- Discard of a non-`var`, non-`Nil` call: point at `e;`, not `let _ = e`.

### 6. Editor grammar

The tree-sitter grammar in `editors/zed` / `tree-sitter-witchy` uses the same
continuing-token table. Then `*slot = 9` on the next line is a statement
without a scanner hack that breaks `n * 2`.

## Alternatives

**Require `;` on every statement.** Stops the climb. Charges the whole
language, including files that never write `&` or `*place`, and fights the
off-side rule. Rejected.

**Strict Python (newline always ends an unbracketed expression).** Also
stops the climb. Breaks leading-dot chains and wrapped `+` / `??` that we
already write. The continuing-token exception is the whole reason to deviate.

**Keep the lookahead and add a case for `&`.** Cheap this week. We will write
the same `if` the next time a prefix operator reuses an infix glyph. Rejected
as policy.

**Give dereference and borrow new glyphs** (`x.*`, `ref x`). Removes the
dual-token clash and leaves statement syntax alone. Worth doing if `&` / `*`
stay confusing in `mode opt` after this ships. It does not help last-line
discard, and it does not remove the match-arm `-` or `.Tag` cases. If §1
lands, the *safety* argument for those glyphs is gone. Keep the alternative
open as taste, not as a load-bearing fix.

**Optional `;` and keep `let _ =`.** Two explicit discards. `fmt` then has to
pick a winner every time, and authors will fight about it. The house rule is
one-cut. `;` wins because it is the mark that also answers "this is not the
block value."

**Do nothing.** The `*` assignment lookahead stays, `&` stays wrong on a
newline, and last-line discard stays a fake binding. Fine until the next
dual glyph.

## Drawbacks

The continuing-token table is still a classification. It is one table, and it
is the same kind of fact `infix_bp` already is. It is not free.

`let x = a` / newline / `* b` silently becomes a dereference if `*b` typechecks.
Worse: `let total = a` / `+ b` / `- c` typechecks and drops `-c`. The
parser has to reject that shape, because authors coming from wrapped
arithmetic will hit it.

People will want `;` after `let`. The parse error has to be early and dull.

Deleting `let _ =` is a one-cut the book, a handful of project files, and the
embedded Rust fixtures have to take with the compiler. The live `.witchy`
corpus is four sites. The teaching corpus is eight sites under `book/`,
`spec/`, and historical RFCs. The fixture corpus is larger and `fmt` cannot
rewrite it.

The `.Tag` residual is still per-construct lookahead. Anyone who reads §1 as
"no more newline special cases" will be wrong, and a later cleanup that
deletes `postfix()`'s uppercase-dot branch will break inline union matches.

## Prior art

Python's implicit line joining is the newline half: a physical newline ends a
logical line except inside brackets. We keep that, then continue across a
newline when the next token cannot start a statement, so leading-dot chains
survive.

Rust's optional semicolon is the discard half: `e` as the last form of a
block is the value; `e;` is `()`. Mid-block, Rust treats `e;` as a statement.
We do the same and we do *not* take Rust's "semicolon after every `let`."

Go's and JavaScript's automatic semicolon insertion are the thing this RFC
is written to avoid. Those insert a terminator the author did not write, using
rules nobody can keep in their head. This RFC *stops* an expression at a
newline when the next token can start a statement. It never inserts `;`.

RFC-0043 is the discard policy we are respelling. RFC-0122 is the prefix
reuse that made the missing newline rule loud.

## Staging

1. Parser and checker grow the continuing-token table, consulted from both
   `expr()` and `postfix()`. The `is_assignment()` and match-arm `-` newline
   branches leave `expr()`. The same-line `[` / `(` guards stay; they do
   not fold into the table. The inline-arm `.Tag` branch in `postfix()`
   stays. The `?` message operand stays same-line. The parser rejects a
   broken wrapped multiply, a wrapped sum, or a next-line borrow in the
   same cut. This step is breaking for any unbracketed dual-glyph
   continuation; that form is rare on purpose.
2. Lexer grows `Tok::Semi`. Parser accepts `;` only as the terminator of an
   expression statement. `let _ = e` becomes a diagnostic that names `e;`.
   `fmt` rewrites the four live `.witchy` sites and the book/spec examples.
   A fixture sweep rewrites witchy source embedded in Rust tests
   (`analysis.rs`, `async_lower.rs`, `src/example_tests/*`,
   `diagnostic_golden_tests.rs`, `loans_tests.rs`, `tests/typeck.rs`,
   `lsp_tests.rs`) and regenerates goldens. Step 2 lands red without that
   sweep.
3. Tree-sitter / Zed pick up the same table so highlighting matches the
   compiler.

Step 1 can land without step 2. Step 2 without step 1 leaves the `*`
lookahead in place and still helps last-line discard. The intended ship is
both.

## Acceptance

- `let slot = select(&mut pair, true)` / newline / `*slot = 9` parses as two
  statements on both backends, with no `is_assignment()` special case.
- `let view = &text` / newline / `&other` is two statements, not bitwise-and.
- `json.get(src, "files")` / newline / `.and_then(...)` remains one
  expression.
- An inline match of the form `.Ok(v) -> v` / newline / `.Err(e) -> e` still
  parses as two arms. Lowercase `.method` after an arm body still chains.
  The `postfix()` uppercase-dot branch remains.
- `e` / newline / `? "msg"` is the message form of `?` on both backends.
  The operand stays on `?`'s line. `e?` / newline / `"msg"` is bare `e?`
  plus a string statement, unchanged from `on_same_line_as_prev()` at
  `parser.rs:1802`.
- `let x = a` / newline / `* b` with `*` meant as multiply is a parse
  error that names the one-line and parenthesized forms.
- `let total = a` / newline / `+ b` / newline / `- c` is a parse error, not
  a silent `a + b` that drops `-c`. Parenthesized `(a + b - c)` remains one
  expression on both backends.
- `fn ne(self: T, other: T) -> Bool:` / newline / `!eq(self, other)` is
  not an error, and neither is `_ ->` / newline / `-1`. The trigger
  requires the previous line to have ended a complete expression. `:` and
  `->` do not. The ten `ne` impls in `std/cmp.witchy` stay legal.
- `xs.length();` as the last form of a `-> Nil` function typechecks on both
  backends; the same call without `;` is the block's `Int`.
- `let x = 1;` is a parse error. `f(a; b)` is a parse error. `";"` in a
  string and `;` in a comment are unchanged.
- `let _ = e` is a diagnostic that points at `e;`. No `let _ =` *discard*
  sites remain under `std/`, `examples/`, `projects/`, `book/`, `spec/`, or
  witchy source embedded in the Rust fixtures listed in Staging step 2.
  `let Point(_, y) = p` is unaffected. Rust `let _ =` in native test
  harnesses is unaffected.
- `witchy fmt` rewrites `let _ = e` to `e;` and does not insert `;` on
  `let`, on a mid-block `var` call, or to satisfy a `Nil` return.
- The tree-sitter parse of the RFC-0122 `*slot = 9` fixture has no `ERROR`
  node, and `n * 2` still parses as multiply.
