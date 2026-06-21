; Witchy syntax highlighting.
; Zed resolves overlapping captures with later patterns winning, so the general
; rules come first and the more specific identifier roles are placed last.

; ---- Comments & literals -------------------------------------------------

(comment) @comment

(integer) @number
(float) @number
(duration) @number
(boolean) @boolean
(string) @string
(escape_sequence) @string.escape

; String interpolation `${ ... }`: clear @string over the interpolation and mark
; the delimiters, so the embedded expression picks up the normal code rules below
; (which, coming later, win over @string for their own ranges).
(interpolation) @none
(interpolation
  "${" @punctuation.special
  "}" @punctuation.special)

; ---- Identifiers: general default, refined below -------------------------

(identifier) @variable

; Types and constructors are capitalised.
((identifier) @type
  (#match? @type "^[A-Z]"))

; ---- Keywords ------------------------------------------------------------

[
  "import" "type" "actor" "on"
  "trait" "impl" "where"
  "fn" "gen" "pub" "let" "var" "update" "spawn"
  "return" "yield"
  "if" "else" "match"
  "for" "while" "in"
  "sink" "own" "move"
  "as" "retain" "without" "region"
  "comptime" "derive"
] @keyword

; `break`/`continue` are whole-rule tokens, so capture the statement nodes.
[
  (break_statement)
  (continue_statement)
] @keyword

; ---- Operators & punctuation --------------------------------------------

[
  "+" "-" "*" "/" "%"
  "==" "!=" "<" ">" "<=" ">="
  "&&" "||" "!"
  "&" "|" "^" "~" "<<" ">>"
  "|>"
  "=" "+=" "-=" "*=" "/=" "%="
  "->" ".." "..=" "?"
] @operator

["(" ")" "[" "]"] @punctuation.bracket
["," ":" "."] @punctuation.delimiter

; ---- Refined identifier roles (last so they take precedence) -------------

(parameter name: (identifier) @variable.parameter)

(record_field name: (identifier) @property)
(actor_field name: (identifier) @property)
(field_assignment field: (identifier) @property)
(field_access field: (identifier) @property)

(variant name: (identifier) @constructor)
(constructor_pattern name: (identifier) @constructor)

(function_definition name: (identifier) @function)
(method_signature name: (identifier) @function)
(handler name: (identifier) @function)

; Traits, impls, and `where` bounds name types.
(trait_definition name: (identifier) @type)
(impl_definition trait: (identifier) @type)
(impl_definition type: (identifier) @type)
(bound trait: (identifier) @type)

; Calls: lowercase are functions, capitalised are constructors, `a.b(...)` is a
; method. These override the @property/@type defaults for the called name.
(call
  function: (identifier) @function
  (#match? @function "^[a-z]"))
(call
  function: (identifier) @constructor
  (#match? @constructor "^[A-Z]"))
(call
  function: (field_access field: (identifier) @function))
