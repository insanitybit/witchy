//! Checker refusals, driven through the public `witchy_types::typeck` API.

    use witchy_types::typeck::*;
    use witchy_syntax::ast::{self, Item};

    fn check_source(source: &str) -> Result<(), TypeError> {
        let module = witchy_syntax::parser::parse_module(source).expect("source parses");
        check(&module)
    }

    #[test]
    fn call_scoped_access_follows_aliases_aggregates_and_captures() {
        for source in [
            "fn escape(let text: String) -> String:\n    let alias = text\n    alias\n",
            "fn escape(let text: String) -> (String, Int):\n    (text, 1)\n",
            "fn escape(let text: String) -> fn() -> String:\n    fn(): text\n",
            "fn escape(let xs: List(Int)) -> &Int:\n    &xs[0]\n",
            "fn callback_escape():\n    fn(let text: String): text\n",
            "fn callback_wrapper():\n    fn(let text: String): (text, 1)\n",
            "fn callback_view(let text: String, callback: fn(let String) -> &String) -> &String:\n    callback(text)\n",
            "fn loop_escape(let text: String, n: Int) -> String:\n    var first = \"\"\n    var second = \"\"\n    var i = 0\n    while i < n:\n        second = first\n        first = text\n        i = i + 1\n    second\n",
        ] {
            let error = check_source(source).expect_err("call-scoped access cannot escape");
            assert!(error.message.contains("call-scoped `let` parameter access"), "{error:?}");
        }

        check_source("fn size(let text: String) -> Int:\n    text.length()\n")
            .expect("a computed scalar does not retain call-scoped access");
        check_source("fn scalar(let value: Int) -> Int:\n    value\n")
            .expect("an inline scalar parameter carries no retained access");
        check_source("fn copy(let text: String) -> String:\n    let independent = text.owned()\n    independent\n")
            .expect("checked explicit materialization establishes an independent value");
        check_source("fn first(xs: List(Int)) -> &Int:\n    &xs[0]\n")
            .expect("an owned input can retain a view of its snapshot");
        check_source("fn callback_size():\n    fn(let text: String): text.length()\n")
            .expect("a callback may compute a scalar from call-scoped access");
        check_source("fn copy_through_callback(let text: String) -> String:\n    let callback = fn(let value: String) -> String:\n        value.owned()\n    callback(text)\n")
            .expect("a checked callback result with its own token is independent of a borrowed argument");
    }

    #[test]
    fn pure_function_value_calls_require_a_pure_contract() {
        check_source(
            "pure fn invoke(callback: pure fn(Int) -> Int, value: Int) -> Int:\n    callback(value)\n",
        )
        .expect("a pure callable may invoke a callback with a declared-pure contract");

        let ordinary = check_source(
            "pure fn invoke(callback: fn(Int) -> Int, value: Int) -> Int:\n    callback(value)\n",
        )
        .expect_err("ordinary callback effects are opaque at a pure call site");
        assert!(ordinary.message.contains("ordinary function value `callback`"), "{ordinary:?}");

        let applied = check_source(
            "pure fn choose(flag: Bool, left: fn(Int) -> Int, right: fn(Int) -> Int) -> Int:\n    (if flag: left else: right)(1)\n",
        )
        .expect_err("Apply must enforce the callable qualifier too");
        assert!(applied.message.contains("apply an ordinary function value"), "{applied:?}");

        check_source(
            "pure fn choose(flag: Bool, left: pure fn(Int) -> Int, right: pure fn(Int) -> Int) -> Int:\n    (if flag: left else: right)(1)\n",
        )
        .expect("Apply accepts an expression whose joined callable contract remains pure");
    }

    #[test]
    fn pure_intrinsics_reject_dynamic_task_toolchain_and_capability_effects() {
        for (source, expected) in [
            (
                "pure fn inspect():\n    let ignored = __dynamic_runtime_type(\"module\", \"Type\")\n",
                "dynamic behavior",
            ),
            (
                "pure fn schedule():\n    __channel_open(1)\n",
                "task scheduling",
            ),
            (
                "import compiler\n\npure fn audit(source: String) -> String:\n    compiler.footprint(source)\n",
                "compiler/toolchain access",
            ),
            (
                "import secretstore\n\npure fn read(store: SecretStore) -> Option(Secret):\n    secretstore.get(store, \"token\")\n",
                "capability authority",
            ),
        ] {
            let error = check_source(source).expect_err("the intrinsic effect is outside pure");
            assert!(error.message.contains(expected), "{error:?}");
        }
    }

    #[test]
    fn pure_closure_boundaries_reject_authority_and_opaque_behavior_captures() {
        let capability = check_source(
            "pure fn defer(console: Console) -> pure fn() -> Int:\n    pure fn():\n        let ignored = console\n        1\n",
        )
        .expect_err("a pure closure cannot capture a host capability");
        assert!(capability.message.contains("carries capability `Console`"), "{capability:?}");

        let ordinary_callback = check_source(
            "pure fn defer(callback: fn() -> Int) -> fn() -> Int:\n    fn(): callback()\n",
        )
        .expect_err("pure construction cannot delegate captured opaque behavior");
        assert!(ordinary_callback.message.contains("ordinary callable with opaque effects"), "{ordinary_callback:?}");

        let pure_lambda_body = check_source(
            "fn ordinary() -> Int:\n    1\n\nfn factory() -> pure fn() -> Int:\n    pure fn(): ordinary()\n",
        )
        .expect_err("a declared-pure lambda body gets its own enforced pure context");
        assert!(pure_lambda_body.message.contains("ordinary function `ordinary`"), "{pure_lambda_body:?}");

        check_source(
            "fn ordinary() -> Int:\n    1\n\npure fn factory() -> Int:\n    let deferred = fn(): ordinary()\n    0\n",
        )
        .expect("an ordinary nested lambda resets latent purity and may defer opaque behavior without capturing authority");

        check_source(
            "pure fn keep(callback: pure fn(Console) -> Int) -> Int:\n    let deferred = pure fn():\n        let ignored = callback\n        1\n    deferred()\n",
        )
        .expect("a pure callable capture is safe and its capability-bearing signature is not captured storage");

        check_source(
            "pure fn defer(callback: pure fn(Int) -> Int) -> Int:\n    let deferred = pure fn(): callback(1)\n    0\n",
        )
        .expect("a pure closure may invoke a captured callback with an explicit pure contract");

    }

    #[test]
    fn pure_closure_capture_is_not_erased_by_a_later_local_shadow() {
        let shadow_laundering = check_str(
            "pure fn defer(console: Console) -> fn() -> ():\n    fn():\n        console.print(\"hidden\")\n        let console = 0\n",
        )
        .expect_err("a later local shadow must not erase an earlier capability capture");
        assert!(shadow_laundering.contains("closure capture `console`"), "{shadow_laundering}");
    }

    #[test]
    fn pure_capture_audit_recurses_through_storage_and_marks_every_user_capability_nominal() {
        let aggregate = check_source(
            "type Holder:\n    Holder(fn() -> Int)\n\npure fn defer(holder: Holder) -> fn() -> Int:\n    fn():\n        let ignored = holder\n        1\n",
        )
        .expect_err("an aggregate that stores an ordinary callable cannot cross a pure boundary");
        assert!(aggregate.message.contains("ordinary callable with opaque effects"), "{aggregate:?}");

        let generic = check_source(
            "pure fn defer(value: a) -> pure fn() -> Int:\n    pure fn():\n        let ignored = value\n        1\n",
        )
        .expect_err("an unresolved stored generic may hide authority");
        assert!(generic.message.contains("generic type may store authority"), "{generic:?}");

        let existential = check_source(
            "trait Inspect:\n    fn inspect(self) -> Int\n\npure fn defer(value: dyn Inspect) -> pure fn() -> Int:\n    pure fn():\n        let ignored = value\n        1\n",
        )
        .expect_err("an existential capture may hide ordinary behavior");
        assert!(existential.message.contains("existential behavior"), "{existential:?}");

        let mut positional = witchy_syntax::parser::parse_module(
            "capability Token:\n    label: String\n\npure fn defer(token: Token) -> pure fn() -> Int:\n    pure fn():\n        let ignored = token\n        1\n",
        )
        .expect("capability source parses");
        let Item::Type(token) = &mut positional.items[0] else {
            panic!("first item is the capability declaration")
        };
        token.sealed = false;
        token.variants[0].field_names.clear();
        let capability = check(&positional)
            .expect_err("positional/unsealed capability identity must not depend on its fields");
        assert!(capability.message.contains("nominal type is declared as a capability"), "{capability:?}");

        check_source(
            "type Grow(a):\n    Value(a)\n    More(Grow(List(a)))\n\npure fn inspect(value: Grow(Int)) -> Int:\n    let deferred = pure fn():\n        let ignored = value\n        1\n    0\n",
        )
        .expect("argument-transforming recursive storage must terminate when it carries only data");

        let transformed = check_source(
            "type Grow(a):\n    Value(a)\n    More(Grow((a, fn() -> Int)))\n\npure fn defer(value: Grow(Int)) -> pure fn() -> Int:\n    pure fn():\n        let ignored = value\n        1\n",
        )
        .expect_err("a transformed recursive stored argument must still expose an ordinary callable");
        assert!(transformed.message.contains("ordinary callable with opaque effects"), "{transformed:?}");
    }

    #[test]
    fn must_consume_requires_disposition_on_every_path() {
        let prelude = "must type Ticket:\n    Ticket(Int)\n\nfn make() -> Ticket:\n    Ticket(1)\n\nfn finish(own ticket: Ticket):\n    let _ = 0\n\n";

        let missing = check_source(&format!(
            "{prelude}fn main():\n    let ticket = make()\n"
        ))
        .expect_err("scope exit must reject a live obligation");
        assert!(missing.message.contains("must-consume value `ticket`"));

        let one_branch = check_source(&format!(
            "{prelude}fn run(flag: Bool):\n    let ticket = make()\n    if flag:\n        finish(ticket)\n\nfn main():\n    run(true)\n"
        ))
        .expect_err("one branch cannot discharge an all-path obligation");
        assert!(one_branch.message.contains("must-consume value `ticket`"));

        check_source(&format!(
            "{prelude}fn run(flag: Bool):\n    let ticket = make()\n    if flag:\n        finish(ticket)\n    else:\n        finish(ticket)\n\nfn main():\n    run(true)\n"
        ))
        .expect("both branches consume the obligation");

        check_source(&format!(
            "{prelude}fn score(own ticket: Ticket) -> Int:\n    1\n\nfn run(flag: Bool) -> Int:\n    let ticket = make()\n    let result: Int = if flag:\n        score(ticket)\n    else:\n        score(ticket)\n    result\n\nfn main():\n    let _ = run(true)\n"
        ))
        .expect("expected-type checking isolates moves made by sibling branches");
    }

    #[test]
    fn loop_backedges_cannot_repeat_must_consume_transfers() {
        let prelude = "must type Ticket:\n    Ticket(Int)\n\nfn finish(own ticket: Ticket):\n    let _ = 0\n\n";
        for (loop_name, source) in [
            (
                "for",
                format!(
                    "{prelude}fn main():\n    let ticket = Ticket(1)\n    for _ in 0..2:\n        finish(ticket)\n"
                ),
            ),
            (
                "while",
                format!(
                    "{prelude}fn run(flag: Bool):\n    let ticket = Ticket(1)\n    while flag:\n        finish(ticket)\n"
                ),
            ),
            (
                "while let",
                format!(
                    "{prelude}fn main():\n    let ticket = Ticket(1)\n    let value: Option(Int) = Some(1)\n    while let Some(_) = value:\n        finish(ticket)\n"
                ),
            ),
        ] {
            let error = check_source(&source)
                .expect_err("a must-consume transfer cannot be repeated by a loop backedge");
            assert!(
                error.message.contains("must-consume value `ticket` is transferred")
                    && error.message.contains(loop_name),
                "{error:?}"
            );
        }

        check_source(&format!(
            "{prelude}fn main():\n    var ticket = Ticket(1)\n    for _ in 0..2:\n        finish(ticket)\n        ticket = Ticket(1)\n    finish(ticket)\n"
        ))
        .expect("a loop may reinitialize its mutable must binding before its backedge");

        check_source(
            "fn main():\n    var total = 0\n    for _ in 0..2:\n        total = total + 1\n",
        )
        .expect("ordinary copyable loop state remains valid");
    }

    #[test]
    fn callable_purity_widens_only_toward_ordinary_and_survives_aliases() {
        check_source(
            "pure fn clean(x: Int) -> Int:\n    x\n\nfn invoke(callback: fn(Int) -> Int) -> Int:\n    callback(1)\n\nfn main() -> Int:\n    let widened: fn(Int) -> Int = clean\n    invoke(widened)\n",
        )
        .expect("pure callable should widen to an ordinary reusable callable");

        let narrowing = check_source(
            "pure fn clean(x: Int) -> Int:\n    x\n\nfn run() -> pure fn(Int) -> Int:\n    let widened: fn(Int) -> Int = clean\n    let narrowed: pure fn(Int) -> Int = widened\n    narrowed\n",
        )
        .expect_err("an ordinary alias must not narrow back to pure");
        assert!(narrowing.message.contains("value disagrees"), "{narrowing:?}");

        let ordinary = check_source(
            "fn ordinary(x: Int) -> Int:\n    x\n\nfn require(callback: pure fn(Int) -> Int) -> Int:\n    callback(1)\n\nfn main() -> Int:\n    require(ordinary)\n",
        )
        .expect_err("ordinary callable must not narrow to pure");
        assert!(ordinary.message.contains("expected `pure fn"), "{ordinary:?}");

        let generic = check_source(
            "fn identity(value: a) -> a:\n    value\n\nfn ordinary(x: Int) -> Int:\n    x\n\nfn main():\n    let narrowed: pure fn(Int) -> Int = identity(ordinary)\n",
        )
        .expect_err("generic transport must not launder an ordinary callable into pure");
        assert!(generic.message.contains("pure fn"), "{generic:?}");
    }

    #[test]
    fn once_parameter_conventions_require_transfer_and_reject_borrowed_invocation_or_var() {
        check_source(
            "fn invoke(callback: once fn() -> Int) -> Int:\n    callback()\n\nfn main() -> Int:\n    let callback: once fn() -> Int = once fn() -> Int: 1\n    invoke(move callback)\n",
        )
        .expect("a default immutable parameter owns a once callback after explicit transfer");

        let copied = check_source(
            "fn invoke(callback: once fn() -> Int) -> Int:\n    callback()\n\nfn main() -> Int:\n    let callback: once fn() -> Int = once fn() -> Int: 1\n    invoke(callback)\n",
        )
        .expect_err("a default once parameter may not copy its caller's binding");
        assert!(copied.message.contains("would be copied"), "{copied:?}");

        let borrowed = check_source(
            "fn inspect(let callback: once fn() -> Int) -> Int:\n    callback()\n",
        )
        .expect_err("an explicit borrow may not consume its caller's once callback");
        assert!(
            borrowed.message.contains("borrowed once-callable")
                && borrowed.message.contains("cannot be invoked"),
            "{borrowed:?}"
        );

        let borrowed_match = check_source(
            "type OnceBox:\n    OnceBox(once fn() -> Int)\n\nfn inspect(let boxed: OnceBox) -> Int:\n    match boxed:\n        OnceBox(_) -> 0\n",
        )
        .expect_err("matching an affine wrapper transfers it and cannot consume a borrow");
        assert!(
            borrowed_match
                .message
                .contains("cannot destructure borrowed affine value"),
            "{borrowed_match:?}"
        );

        let variable = check_source(
            "fn replace(var callback: once fn() -> Int) -> Int:\n    callback()\n",
        )
        .expect_err("var once parameters require definite reinitialization, which is not in v1");
        assert!(variable.message.contains("empty write-back slot"), "{variable:?}");

        for source in [
            "fn both(own left: once fn() -> Int, own right: once fn() -> Int):\n    let _ = 0\n\nfn main():\n    let callback: once fn() -> Int = once fn() -> Int: 1\n    both(callback, callback)\n",
            "fn both(own left: once fn() -> Int, own right: once fn() -> Int):\n    let _ = 0\n\nfn main():\n    let invoke = both\n    let callback: once fn() -> Int = once fn() -> Int: 1\n    invoke(callback, callback)\n",
        ] {
            let duplicate_own = check_source(source)
                .expect_err("two implicit-own arguments may not consume the same root");
            assert!(
                duplicate_own.message.contains("already transferred"),
                "{duplicate_own:?}"
            );
        }

        let existential_duplicate = check_str(
            "trait InvokeBoth:\n    fn both(self, own left: once fn() -> Int, own right: once fn() -> Int)\n\ntype Runner:\n    Runner\n\nimpl InvokeBoth for Runner:\n    fn both(self, own left: once fn() -> Int, own right: once fn() -> Int):\n        let _ = 0\n\nfn main():\n    let dynamic = Runner as dyn InvokeBoth\n    let callback: once fn() -> Int = once fn() -> Int: 1\n    dynamic.both(callback, callback)\n",
        )
        .expect_err("existential convention enforcement must reject duplicate own roots");
        assert!(
            existential_duplicate.contains("already transferred"),
            "{existential_duplicate}"
        );
    }

    #[test]
    fn unique_storage_queries_follow_inference_and_recursive_generic_payloads() {
        check_source(
            "type Chain(a):\n    End\n    More(a, List(Chain(a)))\n\nfn copies(value: Chain(Int)) -> (Chain(Int), Chain(Int)):\n    (value, value)\n",
        )
        .expect("recursive ordinary payloads remain copyable");

        let recursive = check_source(
            "type Owner:\n    value: unique String\n\ntype Chain(a):\n    End\n    More(a, List(Chain(a)))\n\nfn copies(own value: Chain(Owner)):\n    let first = value\n    value\n",
        )
        .expect_err("recursive generic payloads retain their unique-storage obligation");
        assert!(recursive.message.contains("after it was transferred")
            || recursive.message.contains("after it was moved"), "{recursive:?}");

        let inferred = check_source(
            "type Owner:\n    value: unique String\n\nfn copies():\n    var values = []\n    values = [Owner(\"one\")]\n    let first = values\n    values\n",
        )
        .expect_err("an initially unresolved collection acquires its element's affinity");
        assert!(inferred.message.contains("after it was transferred")
            || inferred.message.contains("after it was moved"), "{inferred:?}");

        let rollback = check_source(
            "fn rejected() -> List(Int):\n    let value: .{items: List(Int), needed: Int} = .{items: [], extra: 2}\n    value.items\n",
        )
        .expect_err("partially inferred structural conformance must still reject a missing field");
        assert!(rollback.message.contains("missing required field `needed`"), "{rollback:?}");
    }

    #[test]
    fn unique_storage_is_affine_through_values_aggregates_and_calls() {
        let reused_root = check_source(
            "fn duplicate(value: unique String):\n    let moved = value\n    let copied = value\n",
        )
        .expect_err("a unique value transfer consumes its old binding");
        assert!(
            reused_root.message.contains("unique value `value`")
                || reused_root.message.contains("after it was moved"),
            "{reused_root:?}"
        );

        for source in [
            "fn tuple():\n    let value: unique String = \"one\"\n    let pair = (value, 1)\n    let copied = value\n",
            "fn list():\n    let value: unique String = \"one\"\n    let values = [value]\n    let copied = value\n",
            "type Holder:\n    value: unique String\n    tag: Int\n\nfn nominal():\n    let holder = Holder(\"one\", 0)\n    let moved = holder\n    let copied = holder\n",
        ] {
            let error = check_source(source)
                .expect_err("a shell containing unique storage cannot be copied");
            assert!(
                error.message.contains("unique value") || error.message.contains("after it was moved"),
                "{error:?}"
            );
        }

        let extracted = check_source(
            "type Holder:\n    Holder(unique String)\n\nfn unpack(own holder: Holder):\n    match holder:\n        Holder(value) ->\n            let moved = value\n            value\n",
        )
        .expect_err("constructor extraction retains unique provenance");
        assert!(
            extracted.message.contains("unique value `value`")
                || extracted.message.contains("after it was moved"),
            "{extracted:?}"
        );

        let direct = check_source(
            "fn inspect(value: String) -> Int:\n    0\n\nfn main():\n    let value: unique String = \"one\"\n    let _ = inspect(value)\n    let _ = inspect(value)\n",
        )
        .expect_err("weakening a unique handle at an ordinary call consumes it");
        assert!(direct.message.contains("unique value `value`"), "{direct:?}");

        let indirect = check_source(
            "fn inspect(value: String) -> Int:\n    0\n\nfn main():\n    let inspect_indirect = inspect\n    let value: unique String = \"one\"\n    let _ = inspect_indirect(value)\n    let _ = inspect_indirect(value)\n",
        )
        .expect_err("indirect ordinary calls cannot duplicate unique storage");
        assert!(indirect.message.contains("unique value `value`"), "{indirect:?}");

        check_source(
            "fn inspect(value: unique String) -> String:\n    value\n\nfn retain_ordinary() -> String:\n    let ordinary = \"one\"\n    let seen = inspect(ordinary)\n    ordinary\n",
        )
        .expect("ordinary copyable input remains usable when a unique callee repairs its alias");

        check_source(
            "fn replace(own value: unique String) -> String:\n    let moved = value\n    value = \"two\"\n    value\n",
        )
        .expect("a transferred unique binding may be reinitialized before reuse");

        check_source(
            "type Holder:\n    value: unique String\n    tag: Int\n\nfn replace(var holder: Holder):\n    holder.tag = 1\n",
        )
        .expect("an ordinary sibling remains writable beside a unique field");

        check_source(
            "type Holder:\n    value: String\n    tag: Int\n\nfn copy_tag(own holder: unique Holder) -> Int:\n    let first = holder.tag\n    let second = holder.tag\n    first + second\n",
        )
        .expect("an outer unique handle does not make unqualified field reads affine");

        check_source(
            "type Holder:\n    Holder(unique String, Int)\n\nfn copy_sibling(own holder: Holder) -> Int:\n    match holder:\n        Holder(_value, tag) ->\n            let first = tag\n            let second = tag\n            first + second\n",
        )
        .expect("constructor extraction keeps an ordinary sibling copyable");

        check_source(
            "type Inner:\n    Inner(unique String, Int)\n\ntype Holder:\n    Holder(Inner, Int)\n\nfn nested_siblings(own holder: Holder) -> Int:\n    match holder:\n        Holder(Inner(_value, nested_tag), tag) ->\n            let a = nested_tag\n            let b = nested_tag\n            let c = tag\n            let d = tag\n            a + b + c + d\n",
        )
        .expect("nested nominal extraction follows only the unique field");

        check_source(
            "type Holder:\n    Holder((unique String, Int), Int)\n\nfn tuple_sibling(own holder: Holder) -> Int:\n    match holder:\n        Holder((_value, nested_tag), tag) ->\n            nested_tag + nested_tag + tag + tag\n",
        )
        .expect("nested tuple extraction leaves ordinary elements copyable");

        check_source(
            "type Holder:\n    Holder(List(unique String), Int)\n\nfn list_sibling(own holder: Holder) -> Int:\n    match holder:\n        Holder([_value, .._rest], tag) ->\n            tag + tag\n",
        )
        .expect("nested list extraction leaves an ordinary nominal sibling copyable");

        let generic_shell = check_source(
            "type Box(a):\n    Box(a)\n\nfn duplicate(boxed: Box(unique String)):\n    let moved = boxed\n    let copied = boxed\n",
        )
        .expect_err("stored generic unique arguments remain affine");
        assert!(generic_shell.message.contains("unique value `boxed`"), "{generic_shell:?}");

        let generic_extract = check_source(
            "type Box(a):\n    Box(a)\n\nfn extract(own boxed: Box(unique String)):\n    match boxed:\n        Box(value) ->\n            let moved = value\n            value\n",
        )
        .expect_err("generic constructor extraction substitutes unique arguments");
        assert!(generic_extract.message.contains("unique value `value`"), "{generic_extract:?}");

        let nested_generic_extract = check_source(
            "type Wrap(a):\n    Wrap(List(a))\n\nfn extract(own wrapped: Wrap(unique String)):\n    match wrapped:\n        Wrap([value]) ->\n            let moved = value\n            value\n",
        )
        .expect_err("generic extraction substitutes unique arguments below a list shell");
        assert!(
            nested_generic_extract.message.contains("unique value `value`"),
            "{nested_generic_extract:?}"
        );

        let inferred_generic_extract = check_source(
            "type Box(a):\n    Box(a)\n\nfn extract():\n    let source: unique String = \"one\"\n    let boxed = Box(source)\n    match boxed:\n        Box(value) ->\n            let moved = value\n            value\n",
        )
        .expect_err("an inferred aggregate retains unique source arguments for extraction");
        assert!(
            inferred_generic_extract.message.contains("unique value `value`"),
            "{inferred_generic_extract:?}"
        );

        let indexed_unique = check_source(
            "fn duplicate_index():\n    let xs: List(unique String) = [\"one\"]\n    let first = xs[0]\n    let second = xs[0]\n",
        )
        .expect_err("an index projection retains its unique element provenance");
        assert!(indexed_unique.message.contains("unique value `xs`"), "{indexed_unique:?}");

        let projected_unique = check_source(
            "type Box(a):\n    value: a\n    tag: Int\n\nfn duplicate_field(own boxed: Box(unique String)):\n    let first = boxed.value\n    let second = boxed.value\n",
        )
        .expect_err("a generic field projection retains its unique source argument");
        assert!(
            projected_unique.message.contains("unique value `boxed`"),
            "{projected_unique:?}"
        );

        check_source(
            "type Box(a):\n    value: a\n    tag: Int\n\nfn copy_sibling(own boxed: Box(unique String)) -> Int:\n    let first = boxed.tag\n    let second = boxed.tag\n    first + second\n",
        )
        .expect("an ordinary field projection stays copyable beside a unique generic field");

        let or_extract = check_source(
            "type Choice:\n    A(unique String)\n    B(unique String)\n\nfn extract(own choice: Choice):\n    match choice:\n        A(value) | B(value) ->\n            let moved = value\n            value\n",
        )
        .expect_err("or-pattern alternatives retain unique extraction provenance");
        assert!(or_extract.message.contains("unique value `value`"), "{or_extract:?}");

        let anonymous_extract = check_source(
            "fn extract(own choice: .[Text(unique String) | Count(Int)]):\n    match choice:\n        .Text(value) ->\n            let moved = value\n            value\n        .Count(_) ->\n            \"zero\"\n",
        )
        .expect_err("anonymous-union extraction retains unique payload provenance");
        assert!(
            anonymous_extract.message.contains("unique value `value`"),
            "{anonymous_extract:?}"
        );

        check_source(
            "fn copy_count(own choice: .[Text(unique String) | Count(Int)]) -> Int:\n    match choice:\n        .Text(_) ->\n            0\n        .Count(value) ->\n            value + value\n",
        )
        .expect("anonymous-union extraction keeps an ordinary payload copyable");

        check_source(
            "type Phantom(a):\n    Phantom\n\nfn copy(value: Phantom(unique String)):\n    let first = value\n    let second = value\n",
        )
        .expect("a phantom unique argument does not make its shell affine");

        check_source(
            "mode opt\n\nfn inspect(value: String) -> Int:\n    0\n\nfn main():\n    let value: unique String = \"one\"\n    let shared = &value\n    let first = inspect(*shared)\n    let second = inspect(*shared)\n    let _ = first + second\n",
        )
        .expect("shared loans retain a copyable handle and do not consume unique storage");

        let opt_repair = check_source(
            "mode opt\n\nfn inspect(value: unique String) -> Int:\n    0\n\nfn main():\n    let ordinary = \"one\"\n    inspect(ordinary)\n",
        )
        .expect_err("opt mode may not repair an ordinary alias into a unique owner");
        assert!(opt_repair.message.contains("cannot repair ordinary storage"), "{opt_repair:?}");

        for mode in ["", "mode opt\n\n"] {
            let source = format!(
                "{mode}fn inspect(value: String) -> Int:\n    0\n\nfn main():\n    let value: unique String = \"one\"\n    let _ = inspect(value)\n    let _ = inspect(value)\n"
            );
            let error = check_source(&source)
                .expect_err("both source modes consume a unique value at an ordinary transfer");
            assert!(error.message.contains("unique value `value`"), "{error:?}");
        }

        check_source(
            "fn weaken() -> String:\n    let unique: unique String = \"one\"\n    let ordinary: String = unique\n    let first = ordinary\n    let second = ordinary\n    first\n",
        )
        .expect("explicit weakening consumes the unique source but leaves an ordinary destination");

        let weakened_source = check_source(
            "fn reject_reuse():\n    let unique: unique String = \"one\"\n    let ordinary: String = unique\n    unique\n",
        )
        .expect_err("weakening consumes the old unique binding");
        assert!(weakened_source.message.contains("unique value `unique`"), "{weakened_source:?}");

        check_source(
            "fn normal_repair() -> String:\n    let ordinary: String = \"one\"\n    let unique: unique String = ordinary\n    ordinary\n",
        )
        .expect("normal mode may repair ordinary storage into a unique local");

        let opt_local_repair = check_source(
            "mode opt\n\nfn reject_repair():\n    let ordinary: String = \"one\"\n    let unique: unique String = ordinary\n",
        )
        .expect_err("opt mode may not manufacture a unique local from ordinary storage");
        assert!(opt_local_repair.message.contains("cannot repair ordinary storage"), "{opt_local_repair:?}");

        check_source(
            "fn takes_unique(value: unique String) -> Int:\n    0\n\nfn normal_alias_repair() -> String:\n    let f = takes_unique\n    let ordinary = \"one\"\n    let _ = f(ordinary)\n    ordinary\n",
        )
        .expect("normal mode may repair ordinary storage through a function alias");

        let opt_alias_repair = check_source(
            "mode opt\n\nfn takes_unique(value: unique String) -> Int:\n    0\n\nfn reject_alias_repair():\n    let f = takes_unique\n    var ordinary: String = \"one\"\n    let _ = f(ordinary)\n",
        )
        .expect_err("opt mode retains unique requirements through a function alias");
        assert!(
            opt_alias_repair.message.contains("cannot repair ordinary storage"),
            "{opt_alias_repair:?}"
        );

        let opt_lambda_repair = check_source(
            "mode opt\n\nfn reject_lambda_repair():\n    let f: fn(unique String) -> Int = fn(value: unique String) -> Int:\n        0\n    let ordinary = \"one\"\n    let _ = f(ordinary)\n",
        )
        .expect_err("an annotated lambda retains its unique parameter contract");
        assert!(
            opt_lambda_repair.message.contains("cannot repair ordinary storage"),
            "{opt_lambda_repair:?}"
        );

        for mode in ["", "mode opt\n\n"] {
            let source = format!(
                "{mode}fn takes_unique(value: unique String) -> Int:\n    0\n\nfn transfer_alias():\n    let f = takes_unique\n    let value: unique String = \"one\"\n    let _ = f(value)\n    let _ = f(value)\n"
            );
            let error = check_source(&source)
                .expect_err("both modes transfer an existing unique value through an alias");
            assert!(error.message.contains("unique value `value`"), "{error:?}");
        }
    }

    #[test]
    fn root_suspension_frame_cannot_capture_a_live_must_obligation() {
        fn no_comptime(
            _name: &str,
            _module: &mut witchy_syntax::ast::Module,
            _siblings: &[(String, witchy_syntax::ast::Module)],
        ) -> Result<witchy_syntax::origin::OriginTable, String> {
            Ok(witchy_syntax::origin::OriginTable::default())
        }

        let main = witchy_syntax::parser::parse_module(
            "import task\n\nmust type Ticket:\n    Ticket(Int)\n\nfn finish(own ticket: Ticket) -> Int:\n    1\n\nasync fn pause() -> Int:\n    0\n\nasync fn main():\n    let ticket = Ticket(1)\n    let value = pause().await\n    let _ = finish(ticket) + value\n",
        )
        .expect("root-suspension fixture parses");
        let linked = witchy_syntax::linker::link(vec![("main".into(), main)], "main", no_comptime)
            .expect("root-suspension fixture links with std task");
        let error = annotate_checked_source(linked)
            .expect_err("a root continuation cannot hide a must obligation in a replayable Task");
        assert!(
            error
                .message
                .contains("closure capture `ticket` carries a must-consume obligation"),
            "{error:?}"
        );
    }

    #[test]
    fn resolved_capability_types_preserve_rights_when_converted_to_ast() {
        let named = |name: &str| ast::Type::Named(name.to_string(), Vec::new());
        assert_eq!(
            ty_to_ast(&Ty::Dir(DirRights { read: true, write: false })),
            Some(ast::Type::Named("Dir".into(), vec![named("Read")]))
        );
        assert_eq!(
            ty_to_ast(&Ty::File(FileRights { read: false, write: true })),
            Some(ast::Type::Named("File".into(), vec![named("Write")]))
        );
        assert_eq!(
            ty_to_ast(&Ty::Net(NetRights {
                connect: true,
                listen: false,
                tcp: true,
                udp: false,
                uds: false,
            })),
            Some(ast::Type::Named(
                "Net".into(),
                vec![named("Connect"), named("Tcp")],
            ))
        );
        assert_eq!(
            ty_to_ast(&Ty::Dir(DirRights::full())),
            Some(named("Dir")),
        );
        assert_eq!(
            ty_to_ast(&Ty::Dir(DirRights { read: false, write: false })),
            None,
        );
        assert_eq!(
            ty_to_ast(&Ty::File(FileRights { read: false, write: false })),
            None,
        );
        assert_eq!(
            ty_to_ast(&Ty::Net(NetRights {
                connect: false,
                listen: false,
                tcp: true,
                udp: false,
                uds: false,
            })),
            None,
        );
        assert_eq!(
            ty_to_ast(&Ty::Net(NetRights {
                connect: true,
                listen: false,
                tcp: false,
                udp: false,
                uds: false,
            })),
            None,
        );
    }

    #[test]
    fn trait_method_purity_is_a_directed_callable_contract() {
        let missing = "trait Inspect:\n    pure fn inspect(self) -> Int\n\
                       type Box:\n    Box(Int)\n\
                       impl Inspect for Box:\n    fn inspect(self) -> Int:\n        0\n";
        let error = check_str(missing).expect_err("ordinary impl cannot satisfy a pure method");
        assert!(
            error.contains("ordinary, but the trait requires `pure fn`"),
            "{error}"
        );

        let stronger = "trait Inspect:\n    fn inspect(self) -> Int\n\
                        type Box:\n    Box(Int)\n\
                        impl Inspect for Box:\n    pure fn inspect(self) -> Int:\n        0\n";
        check_str(stronger).expect("pure impl may satisfy an ordinary method contract");
        let module = witchy_syntax::parser::parse_module(stronger).expect("parse");
        let lowered = witchy_types::traits::lower_checked(module).expect("lower");
        assert!(lowered.items.iter().any(|item| matches!(
            item,
            witchy_syntax::ast::Item::Function(function)
                if function.name.ends_with("__inspect") && function.pure
        )));

        let pure_dispatch = "trait Inspect:\n    pure fn inspect(self) -> Int\n\
                             type Box:\n    Box(Int)\n\
                             impl Inspect for Box:\n    pure fn inspect(self) -> Int:\n        0\n\
                             pure fn read(boxed: Box) -> Int:\n    boxed.inspect()\n";
        check_str(pure_dispatch)
            .expect("trait lowering must preserve the selected implementation's pure qualifier");

        let existential_dispatch = check_str(
            "trait Inspect:\n    pure fn inspect(self) -> Int\n\npure fn read(value: dyn Inspect) -> Int:\n    value.inspect()\n",
        )
        .expect_err("existential dispatch has no statically selected pure implementation");
        assert!(existential_dispatch.contains("existential method"), "{existential_dispatch}");
    }

    #[test]
    fn capability_methods_keep_method_origin_after_lowering() {
        use witchy_syntax::ast::{Expr, Item, Stmt};

        let src = r#"
fn read(f: File[Read]) -> String:
    "shadow"

fn main(console: Console, f: File[Read]):
    console.print(f.read())
"#;
        check_str(src).expect("file read capability method should type-check");

        let module = witchy_syntax::parser::parse_module(src).expect("parse");
        let lowered = witchy_types::traits::lower_checked(module).expect("lower");
        let main = lowered
            .items
            .iter()
            .find_map(|item| match item {
                Item::Function(f) if f.name == "main" => Some(f),
                _ => None,
            })
            .expect("main");
        let Some(Stmt::Expr(Expr::Call { name, args })) = main.body.stmts.last() else {
            panic!("expected lowered console.print call");
        };
        assert_eq!(name, "__capop.print");
        let Some(Expr::Call { name: inner, .. }) = args.get(1) else {
            panic!("expected lowered f.read call");
        };
        assert_eq!(inner, "__capop.read");
    }

    #[test]
    fn capability_methods_prefer_host_ops_over_std_owner_modules() {
        use witchy_syntax::ast::{Expr, Item, Stmt};

        let src = r#"
fn main(rand: Rand, net: Net[Listen, Tcp]):
    let _ = rand.rand_u64()
    let listener = net.listen("127.0.0.1:0")
    listener.serve_pool()
"#;
        assert!(check_str(src).is_ok());

        let module = witchy_syntax::parser::parse_module(src).expect("parse");
        let lowered = witchy_types::traits::lower_checked(module).expect("lower");
        let main = lowered
            .items
            .iter()
            .find_map(|item| match item {
                Item::Function(f) if f.name == "main" => Some(f),
                _ => None,
            })
            .expect("main");
        let Some(Stmt::LetPattern { value: Expr::Call { name: rand_name, .. }, .. }) = main.body.stmts.first() else {
            panic!("expected lowered rand.rand_u64 call");
        };
        assert_eq!(rand_name, "__capop.rand_u64");
        let Some(Stmt::Expr(Expr::Call { name: serve_name, .. })) = main.body.stmts.last() else {
            panic!("expected lowered listener.serve_pool call");
        };
        assert_eq!(serve_name, "__capop.serve_pool");
    }

    #[test]
    fn capability_op_chains_preserve_receiver_kind() {
        use witchy_syntax::ast::{Expr, Item, Stmt};

        let src = r#"
type DirPolicy:
    Any

fn load(dir: Dir[Read], policy: DirPolicy) -> String:
    dir.only(policy).read("config.txt")
"#;
        check_str(src).expect("Dir.only(...).read(...) should type-check");

        let module = witchy_syntax::parser::parse_module(src).expect("parse");
        let lowered = witchy_types::traits::lower_checked(module).expect("lower");
        let load = lowered
            .items
            .iter()
            .find_map(|item| match item {
                Item::Function(f) if f.name == "load" => Some(f),
                _ => None,
            })
            .expect("load");
        let Some(Stmt::Expr(Expr::Call { name: read_name, args: read_args })) =
            load.body.stmts.last()
        else {
            panic!("expected lowered dir.read call");
        };
        assert_eq!(read_name, "__capop.read");
        let Some(Expr::Call { name: only_name, .. }) = read_args.first() else {
            panic!("expected lowered dir.only receiver");
        };
        assert_eq!(only_name, "__capop.only");
    }
