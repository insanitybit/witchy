/**
 * Tree-sitter grammar for witchy.
 *
 * witchy uses the off-side rule: blocks are introduced by a `:` header and
 * delimited by indentation. An external scanner (src/scanner.c) turns that
 * indentation into `_indent`/`_dedent` tokens; newlines are ordinary whitespace,
 * so statements are self-delimiting and bracketed expressions span lines freely.
 * A single `identifier` token covers variables, functions, types and
 * constructors; the case distinction (Type/Constructor vs value) is made in the
 * highlight queries.
 */

const PREC = {
  pipe: 1,
  range: 2,
  coalesce: 3,
  or: 4,
  and: 5,
  compare: 6,
  bit_or: 7,
  bit_xor: 8,
  bit_and: 9,
  shift: 10,
  add: 11,
  mul: 12,
  unary: 13,
  call: 14,
  try: 15,
};

module.exports = grammar({
  name: 'witchy',

  word: $ => $.identifier,

  extras: $ => [/[ \t\r\f\n]/, $.comment],

  externals: $ => [$._indent, $._dedent, $._statement_break, $._view_amp],

  conflicts: $ => [
    [$.lambda, $._var_identifier],
    [$.update_expression, $._var_identifier],
    [$.quote_expression, $._var_identifier],
  ],

  rules: {
    source_file: $ => seq(
      optional($.mode_clause),
      repeat($._item),
    ),

    comment: _ => token(seq('//', /.*/)),

    // File-level performance mode. Contextual in the compiler (`mode` stays a
    // usable identifier elsewhere); reserved here so the header highlights.
    // Syntax is shared by both modes; the compiler enforces opt-mode contracts.
    mode_clause: $ => seq('mode', commaSep1($.identifier)),

    // ---- Items -----------------------------------------------------------

    _item: $ => choice(
      $.import,
      $.type_definition,
      $.type_alias,
      $.capability_definition,
      $.const_definition,
      $.trait_definition,
      $.impl_definition,
      $.function_definition,
      $.actor_definition,
      $.comptime_block,
    ),

    import: $ => choice(
      seq('import', field('name', $.identifier)),
      seq('from', field('module', $.identifier), 'import', commaSep1(field('name', $.identifier))),
    ),

    capability_definition: $ => seq(
      optional(choice('grantable', 'must')),
      'capability',
      field('name', $.identifier),
      choice(
        seq('from', field('from', $._type)),
        seq(':', field('body', $.type_body)),
      ),
    ),

    // `comptime:` — a compile-time block whose emitted output is appended to
    // the module as new items.
    comptime_block: $ => seq('comptime', ':', field('body', $.block)),

    // Module-level `let MAX = 100` (a constant, inlined at use sites).
    const_definition: $ => seq(
      'let',
      field('name', $.identifier),
      '=',
      field('value', $._expression),
    ),

    // `type Id = Int` / `type Pair(a) = (a, a)` — a transparent alias.
    type_alias: $ => seq(
      'type',
      field('name', $.identifier),
      optional(field('parameters', $.type_parameters)),
      '=',
      field('type', $._type),
    ),

    // Ordinary type parameters only; lifetime parameters are migration syntax.
    type_definition: $ => seq(
      optional(choice('sealed', 'must', seq('must', 'sealed'))),
      'type',
      field('name', $.identifier),
      optional(field('parameters', $.type_parameters)),
      optional(field('derive', $.derive_clause)),
      ':',
      field('body', $.type_body),
    ),
    type_parameters: $ => seq('(', commaSep1($.identifier), ')'),
    derive_clause: $ => seq('derive', '(', commaSep1($.identifier), ')'),

    type_body: $ => seq($._indent, repeat($._type_member), $._dedent),
    _type_member: $ => choice($.record_field, $.variant),
    record_field: $ => seq(field('name', $._var_identifier), ':', field('type', $._type)),
    variant: $ => seq(
      field('name', $.identifier),
      optional(seq(token.immediate('('), commaSep1($._type), ')')),
    ),

    attribute: $ => seq('@', choice('dynamic', 'browser', 'server', 'static', $.identifier)),

    function_definition: $ => seq(
      repeat($.attribute),
      optional('pub'),
      optional('comptime'),
      optional(choice('pure', 'once', seq('pure', 'once'))),
      optional(choice('async', 'gen')),
      'fn',
      field('name', $.identifier),
      field('parameters', $.parameters),
      optional(seq('->', field('return_type', $._type))),
      optional($.where_clause),
      ':',
      field('body', $.block),
    ),
    where_clause: $ => seq('where', commaSep1($.bound)),
    bound: $ => seq(field('var', $.identifier), ':', field('trait', choice($.generic_type, $.identifier))),
    parameters: $ => seq('(', commaSep($.parameter), ')'),
    parameter: $ => seq(
      optional(field('modifier', choice('inout', 'sink', 'own', 'let', 'var'))),
      field('name', $._var_identifier),
      optional(seq(':', field('type', $._type))),
    ),

    trait_definition: $ => seq(
      'trait',
      field('name', $.identifier),
      optional(field('parameters', $.type_parameters)),
      optional(seq(':', field('supertraits', sep1($._supertrait, '+')))),
      optional(seq(':', field('body', $.trait_body))),
    ),
    _supertrait: $ => choice($.generic_type, $.identifier),
    trait_body: $ => seq($._indent, repeat($.method_signature), $._dedent),
    method_signature: $ => seq(
      optional('pub'),
      optional(choice('pure', 'once', seq('pure', 'once'))),
      optional(choice('async', 'gen')),
      'fn',
      field('name', $.identifier),
      field('parameters', $.parameters),
      optional(seq('->', field('return_type', $._type))),
      optional(seq(':', field('default', $.block))),
    ),

    impl_definition: $ => seq(
      'impl',
      optional(seq(field('trait', choice($.generic_type, $.identifier)), 'for')),
      field('type', $._type),
      optional($.where_clause),
      optional(seq(':', field('body', $.impl_body))),
    ),
    impl_body: $ => seq($._indent, repeat(choice($.function_definition, $.handler)), $._dedent),

    actor_definition: $ => seq(
      'actor',
      field('name', $.identifier),
      ':',
      field('body', $.actor_body),
    ),
    actor_body: $ => seq($._indent, repeat($._actor_member), $._dedent),
    _actor_member: $ => choice($.state_field, $.actor_field, $.handler),
    actor_field: $ => seq(field('name', $.identifier), ':', field('type', $._type)),
    state_field: $ => seq(
      'var',
      field('name', $._var_identifier),
      optional(seq(':', field('type', $._type))),
      '=',
      field('value', $._expression),
    ),
    handler: $ => seq(
      'on',
      field('name', $.identifier),
      field('parameters', $.parameters),
      ':',
      field('body', $.block),
    ),

    // ---- Types -----------------------------------------------------------

    _type: $ => $._value_type,
    _value_type: $ => seq(
      optional(field('qualifier', choice('unique', 'frozen'))),
      choice(
        $.unit,
        $.reference_type,
        $.dyn_type,
        $.slice_type,
        $.generic_type,
        $.rights_type,
        $.qualified_type,
        $.tuple_type,
        $.function_type,
        $.impl_trait_type,
        $.anon_union_type,
        $.identifier,
      ),
    ),
    dyn_type: $ => seq('dyn', field('trait', choice($.generic_type, $.identifier))),
    slice_type: $ => seq('[', field('element', $._type), ']'),
    qualified_type: $ => seq(field('module', $.identifier), '.', field('name', $.identifier)),
    // RFC-0167: immutable rooted views are the only reference spelling.
    // `&[T]` is a view of the slice type, not a distinct legacy loan.
    reference_type: $ => prec.right(seq(
      $._view_amp,
      field('type', $._type),
    )),
    // `x: impl Show` — a fresh type variable bounded by the trait.
    impl_trait_type: $ => seq('impl', field('trait', choice($.generic_type, $.identifier))),
    generic_type: $ => prec(1, seq(
      field('name', choice(seq($.identifier, '.', $.identifier), $.identifier)),
      token.immediate('('),
      commaSep1($._type),
      ')',
    )),
    // Capability rights: `Dir[Read]`, `Net[Connect, Tcp]`.
    rights_type: $ => prec(1, seq(
      field('name', $.identifier),
      token.immediate('['),
      commaSep1($.identifier),
      ']',
    )),
    tuple_type: $ => seq('(', $._type, ',', commaSep1($._type), ')'),
    function_type: $ => seq(
      optional(choice('pure', 'once')),
      'fn',
      '(',
      commaSep($.function_type_parameter),
      ')',
      '->',
      field('return_type', $._type),
    ),
    function_type_parameter: $ => seq(
      optional(field('modifier', choice('inout', 'sink', 'own', 'let', 'var'))),
      optional(seq(field('name', $._var_identifier), ':')),
      field('type', $._type),
    ),
    anon_union_type: $ => seq('.[', sep1($.variant, '|'), ']'),

    // ---- Statements ------------------------------------------------------

    block: $ => seq($._indent, repeat(choice($._statement, $._statement_break)), $._dedent),
    _statement: $ => choice(
      $.let_statement,
      $.var_statement,
      $.return_statement,
      $.yield_statement,
      $.break_statement,
      $.continue_statement,
      $.for_statement,
      $.while_statement,
      $.assignment,
      $.expression_statement,
    ),
    yield_statement: $ => seq('yield', field('value', $._expression)),
    let_statement: $ => seq(
      'let',
      field('pattern', $._pattern),
      optional(seq(':', field('type', $._type))),
      '=',
      field('value', $._expression),
    ),
    var_statement: $ => seq(
      'var',
      field('name', $._var_identifier),
      optional(seq(':', field('type', $._type))),
      '=',
      field('value', $._expression),
    ),
    return_statement: $ => prec.right(seq('return', optional($._expression), optional(seq('if', field('condition', $._expression))))),
    break_statement: _ => 'break',
    continue_statement: _ => 'continue',
    for_statement: $ => seq(
      'for',
      optional(choice('await', 'var')),
      field('pattern', commaSep1($._pattern)),
      'in',
      field('iterable', $._expression),
      ':',
      field('body', $.block),
    ),
    while_statement: $ => seq(
      'while',
      optional(seq('let', field('pattern', $._pattern), '=')),
      field('condition', $._expression),
      ':',
      field('body', $.block),
    ),
    assignment: $ => prec.right(seq(
      field('target', $._expression),
      field('operator', choice('=', '+=', '-=', '*=', '/=', '%=')),
      field('value', $._expression),
    )),
    expression_statement: $ => $._expression,

    // ---- Expressions -----------------------------------------------------

    _expression: $ => choice(
      $.unit,
      $.integer,
      $.float,
      $.duration,
      $.string,
      $.boolean,
      $._var_identifier,
      $.dyn_expression,
      $.list,
      $.tuple,
      $.parenthesized,
      $.anon_struct,
      $.anon_union,
      $.unary_expression,
      $.binary_expression,
      $.range,
      $.call,
      $.await_expression,
      $.index,
      $.field_access,
      $.lambda,
      $.spawn,
      $.update_expression,
      $.quote_expression,
      $.if_expression,
      $.match_expression,
      $.try_expression,
      $.move_expression,
      $.as_expression,
      $.restrict_expression,
      $.region_expression,
    ),

    await_expression: $ => prec(PREC.call, seq(
      field('argument', $._expression),
      '.',
      'await',
    )),

    anon_struct: $ => seq(
      choice('.{', seq('.', '{')),
      commaSep(choice($.named_argument, $.spread_argument)),
      '}',
    ),

    anon_union: $ => seq(
      '.',
      field('constructor', $.identifier),
    ),

    dyn_expression: $ => seq('dyn', field('trait', choice($.generic_type, $.identifier))),

    quote_expression: $ => choice(seq(
      'quote', field('category', 'type'), ':',
      $._indent, field('body', $._type), $._dedent,
    ), seq(
      'quote', field('category', 'item'), ':',
      $._indent, field('body', $._item), $._dedent,
    ), seq(
      'quote',
      field('category', choice('expr', 'pattern', 'stmt', 'block')),
      ':',
      field('body', choice($.block, $._expression)),
    )),

    // `move x` — explicit ownership transfer at a call site.
    move_expression: $ => prec(PREC.unary, seq('move', field('value', $._expression))),

    // `cap as Dir[Read]` — capability narrowing (and type ascription).
    as_expression: $ => prec.left(PREC.compare, seq(
      field('value', $._expression),
      'as',
      field('type', $._type),
    )),

    // `retain a, b:` / `without c:` — a capability firewall for a block.
    restrict_expression: $ => seq(
      field('mode', choice('retain', 'without')),
      commaSep($.identifier),   // `retain:` with no names = fully sealed
      ':',
      field('body', $.block),
    ),

    // `region:` / `region -> Type:` — a user-controlled allocation scope.
    region_expression: $ => seq(
      'region',
      optional(seq('->', field('type', $._type))),
      ':',
      field('body', $.block),
    ),

    unit: _ => seq('(', ')'),
    parenthesized: $ => seq('(', $._expression, ')'),
    tuple: $ => seq('(', $._expression, ',', commaSep1($._expression), ')'),

    list: $ => seq('[', optional(choice($.comprehension, commaSep1($._expression))), ']'),
    comprehension: $ => seq(
      field('body', $._expression),
      repeat1($.comprehension_clause),
    ),
    comprehension_clause: $ => choice(
      seq('for', field('pattern', commaSep1($._pattern)), 'in', field('iterable', $._expression)),
      seq('if', field('condition', $._expression)),
    ),

    call: $ => prec(PREC.call, seq(
      field('function', $._expression),
      token.immediate('('),
      commaSep(choice($._expression, $.named_argument, $.spread_argument)),
      ')',
    )),
    // Named-field record construction (`Account(name: "x", ..base)`).
    named_argument: $ => seq(field('name', $.identifier), ':', field('value', $._expression)),
    spread_argument: $ => seq('..', field('base', $._expression)),
    // Subscript indexing: `xs[i]`, chains as `grid[r][c]`.
    index: $ => prec(PREC.call, seq(
      field('value', $._expression),
      token.immediate('['),
      field('index', $._expression),
      ']',
    )),
    field_access: $ => prec(PREC.call, seq(
      field('record', $._expression),
      '.',
      // An identifier names a record field; an integer reads a tuple element
      // by position (`pair.0`, chaining as `grid.0.1`).
      field('field', choice($.identifier, $.integer)),
    )),
    try_expression: $ => prec(PREC.try, seq($._expression, '?')),

    // A lambda body is a single inline expression (`fn(x): x + 1`) or an
    // indented block (`fn(x):` then an indented body).
    lambda: $ => prec.right(seq(
      optional(choice('pure', 'once', seq('pure', 'once'))),
      'fn',
      field('parameters', $.parameters),
      optional(seq('->', field('return_type', $._type))),
      ':',
      field('body', choice($.block, $._expression)),
    )),
    spawn: $ => prec.right(seq('spawn', $._expression)),
    // Brace-free record update: `update e: f = v g = w`.
    update_expression: $ => prec.right(seq(
      'update',
      field('record', $._expression),
      ':',
      repeat1($.field_assignment),
    )),
    field_assignment: $ => seq(field('field', $.identifier), '=', field('value', $._expression)),

    // Inline `if c: a else: b`, or block form `if c:` then an indented body.
    // `else if ...` chains (no `:` after that `else`).
    if_expression: $ => prec.right(seq(
      'if',
      // `if let PAT = SCRUT:` — pattern-or-fallback sugar for a match.
      optional(seq('let', field('pattern', $._pattern), '=')),
      field('condition', $._expression),
      ':',
      field('consequence', choice($.block, $._expression)),
      optional(seq('else', choice(
        field('alternative', $.if_expression),
        seq(':', field('alternative', choice($.block, $._expression))),
      ))),
    )),
    match_expression: $ => seq('match', field('value', $._expression), ':', field('body', $.match_block)),
    match_block: $ => seq($._indent, repeat($.match_arm), $._dedent),
    match_arm: $ => prec.right(seq(
      field('pattern', $._pattern),
      optional(seq('if', field('guard', $._expression))),
      '->',
      // An inline body may also be a single statement (`-> return e`,
      // `-> x = e`, `-> break`).
      field('body', choice(
        $.block,
        $.assignment,
        $.return_statement,
        $.break_statement,
        $.continue_statement,
        $._expression,
      )),
    )),

    unary_expression: $ => prec(PREC.unary, choice(
      seq(
        field('operator', choice('-', '!', '~', '*')),
        field('operand', $._expression),
      ),
      // RFC-0167 forms a rooted immutable view of a place.
      seq(
        $._view_amp,
        field('operand', $._expression),
      ),
    )),
    binary_expression: $ => {
      const table = [
        ['|>', PREC.pipe],
        ['??', PREC.coalesce],
        ['||', PREC.or],
        ['&&', PREC.and],
        ['==', PREC.compare], ['!=', PREC.compare],
        ['<', PREC.compare], ['>', PREC.compare], ['<=', PREC.compare], ['>=', PREC.compare],
        ['|', PREC.bit_or],
        ['^', PREC.bit_xor],
        ['&', PREC.bit_and],
        ['<<', PREC.shift], ['>>', PREC.shift],
        ['+', PREC.add], ['-', PREC.add],
        ['*', PREC.mul], ['/', PREC.mul], ['%', PREC.mul],
      ];
      return choice(...table.map(([op, p]) => prec.left(p, seq(
        field('left', $._expression),
        field('operator', op),
        field('right', $._expression),
      ))));
    },
    range: $ => prec.left(PREC.range, seq($._expression, choice('..', '..='), $._expression)),

    // ---- Patterns --------------------------------------------------------

    _pattern: $ => choice(
      $.unit,
      $.integer,
      $.float,
      $.string,
      $.boolean,
      $.negative_pattern,
      $._var_identifier,
      $.constructor_pattern,
      $.tuple_pattern,
      $.list_pattern,
      $.or_pattern,
    ),
    negative_pattern: $ => seq('-', choice($.integer, $.float)),
    constructor_pattern: $ => seq(
      field('name', choice(seq($.identifier, '.', $.identifier), $.identifier)),
      token.immediate('('),
      commaSep($._pattern),
      ')',
    ),
    tuple_pattern: $ => seq('(', $._pattern, ',', commaSep1($._pattern), ')'),
    list_pattern: $ => seq('[', commaSep($._list_element), ']'),
    _list_element: $ => choice($._pattern, $.rest_pattern),
    rest_pattern: $ => seq('..', optional($.identifier)),
    or_pattern: $ => prec.left(seq($._pattern, '|', $._pattern)),

    // ---- Tokens ----------------------------------------------------------

    _var_identifier: $ => choice(
      $.identifier,
      'pure',
      'once',
      'sealed',
      'must',
      'grantable',
      'mode',
      'derive',
      'unique',
      'frozen',
      'update',
      'quote',
    ),

    identifier: _ => /[a-zA-Z_][a-zA-Z0-9_]*/,
    boolean: _ => choice('true', 'false'),
    integer: _ => token(choice(
      /0[xX][0-9a-fA-F_]+/,
      /0[bB][01_]+/,
      /[0-9][0-9_]*/,
    )),
    float: _ => token(/[0-9][0-9_]*\.[0-9][0-9_]*/),
    // Native duration literals: 30s, 250ms, 5m, 2h/2hr, 1d, 1w.
    duration: _ => token(/[0-9][0-9_]*(ms|hr|s|m|h|d|w)/),

    string: $ => seq(
      '"',
      repeat(choice($.escape_sequence, $.interpolation, $.string_content)),
      '"',
    ),
    string_content: _ => token.immediate(prec(1, /[^"\\$]+|\$/)),
    escape_sequence: _ => token.immediate(/\\["\\ntr0$]/),
    // `${` must outrank string_content's lone-`$` (prec 1) so interpolation
    // wins at `${`, while a bare `$` (e.g. "price $5") still reads as literal.
    interpolation: $ => seq(token.immediate(prec(2, '${')), $._expression, '}'),
  },
});

function commaSep(rule) {
  return optional(commaSep1(rule));
}

function commaSep1(rule) {
  return seq(rule, repeat(seq(',', rule)), optional(','));
}

function sep1(rule, separator) {
  return seq(rule, repeat(seq(separator, rule)));
}
