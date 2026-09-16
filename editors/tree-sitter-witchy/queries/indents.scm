; Off-side rule: an indented block follows a `:` header, so the indentation
; level tracks these block nodes. Bracketed constructs indent until their close.
[
  (block)
  (type_body)
  (trait_body)
  (impl_body)
  (actor_body)
  (match_block)
] @indent

(parameters ")" @end) @indent
(list "]" @end) @indent
(tuple ")" @end) @indent
