//! Capability delegation and recursively carried reference origins.
use crate::diagnostic::Diagnostic;

fn checked(source: &str) -> Result<(), Diagnostic> {
    let logical = super::check::program(&super::parse(source)?)?;
    super::concrete::program(logical)?;
    Ok(())
}
fn refused(source: &str, message: &str) {
    let error = checked(source).expect_err(source);
    assert!(
        error.message.contains(message),
        "{}: {source}",
        error.message
    );
}
const MAIN: &str = "\nfn main():\n    return\n";
const SEALED: &str = r#"sealed type Gauge:
    count: U64

sealed type Vault:
    gauge: Gauge
    slots: [U64; 2]

sealed type Store:
    data: Buffer<U64>

type Holder:
    vault: Vault

type Open:
    gauge: Gauge

impl Gauge:
    fn reset(self: &mut Gauge):
        (*self).count = 0

fn overwrite(slot: &mut U64):
    *slot = 1

fn relay(slot: &mut U64):
    overwrite(slot)

"#;
fn modules(library: &str, client: &str) -> Result<(), Diagnostic> {
    let library = &super::exported(library);
    let source = format!("{library}import 0\n{client}{MAIN}");
    let syntax = super::parse_modules(&source, &[0, library.len()])?;
    let logical = super::check::program(&syntax)?;
    super::concrete::program(logical)?;
    Ok(())
}

#[test]
fn sealed_ancestors_protect_explicit_reborrow_method_and_intrinsic_routes() {
    for client in [
        "fn tamper(v: &mut Gauge):\n    overwrite(&mut (*v).count)\n",
        "fn tamper(v: &mut Gauge):\n    relay(&mut (*v).count)\n",
        "fn tamper(v: Gauge):\n    var own = v\n    overwrite(&mut own.count)\n",
        "fn tamper(v: &mut Holder):\n    overwrite(&mut (*v).vault.gauge.count)\n",
        "fn tamper(v: &mut Vault):\n    overwrite(&mut (*v).slots[0])\n",
        "fn tamper(v: &mut Vault):\n    (*v).gauge.reset()\n",
        "fn tamper(v: &mut Vault):\n    Gauge.reset(&mut (*v).gauge)\n",
        "fn tamper(v: &mut Store):\n    let _ = memory.init(&mut (*v).data, 0, 1)\n",
        "fn tamper(v: &mut Store):\n    let _ = memory.take(&mut (*v).data, 0)\n",
        "fn tamper(v: &mut Store):\n    memory.swap(&mut (*v).data, 0, 1)\n",
        "fn tamper(v: &mut Store):\n    memory.init_unchecked(&mut (*v).data, 0, 1)\n",
        "fn tamper(v: &mut Store):\n    memory.take_unchecked(&mut (*v).data, 0)\n",
        "fn tamper(v: &mut Store):\n    memory.try_init(&mut (*v).data, 0, 1)\n",
    ] {
        let error = modules(SEALED, client).expect_err(client);
        assert!(
            error.message.contains("sealed type"),
            "{}: {client}",
            error.message
        );
    }
}

#[test]
fn whole_sealed_public_apis_shared_fields_and_unsealed_ancestors_remain_valid() {
    modules(
        SEALED,
        r#"fn read(slot: &U64) -> U64:
    return *slot

fn public(v: &mut Gauge) -> U64:
    v.reset()
    Gauge.reset(v)
    return read(&(*v).count)

fn nested(v: &mut Open):
    (*v).gauge.reset()
"#,
    )
    .unwrap();
    // Within the defining file a projection can delegate mutation; an open
    // representation has the same permissions when used from another file.
    let client = "fn local(v: &mut Vault):\n    overwrite(&mut (*v).gauge.count)\n";
    checked(&format!("{SEALED}{client}{MAIN}")).unwrap();
    modules(&SEALED.replace("sealed type", "type"), client).unwrap();
}

const NESTED: &str = r#"type Deep = Result<Result<&U64, U64>, U64>

fn wrap(value: &U64) -> Deep:
    return Ok(Ok(value))

fn pass(value: Deep) -> Deep:
    return value

fn read(value: Deep) -> U64:
    match value:
        Ok(inner):
            match inner:
                Ok(item):
                    return *item
                Err(code):
                    return code
        Err(code):
            return code

"#;

#[test]
fn nested_sums_reject_local_origins_on_direct_bound_and_matched_returns() {
    for body in [
        "    let x: U64 = 41\n    return Ok(Ok(&x))\n",
        "    let x: U64 = 41\n    let held = wrap(&x)\n    return held\n",
        "    let x: U64 = 41\n    return pass(wrap(&x))\n",
        "    let x: U64 = 41\n    match wrap(&x):\n        Ok(inner):\n            return Ok(inner)\n        Err(code):\n            return Err(code)\n",
    ] {
        refused(
            &format!("{NESTED}fn escape() -> Deep:\n{body}{MAIN}"),
            "must borrow from a reference parameter",
        );
    }
    refused(
        &format!("{NESTED}fn escape(x: &mut U64) -> Deep:\n    return Ok(Ok(&*x))\n{MAIN}"),
        "borrows only with `&`",
    );
}

#[test]
fn nested_sum_bindings_copies_calls_and_matches_keep_the_source_lent() {
    for statements in [
        "    let found = wrap(&x)\n    x = 99\n    return read(found)\n",
        "    let found = wrap(&x)\n    let copy = found\n    x = 99\n    return read(copy)\n",
        "    let found = pass(wrap(&x))\n    x = 99\n    return read(found)\n",
        "    let found: Deep = Ok(Ok(&x))\n    x = 99\n    return read(found)\n",
        "    match wrap(&x):\n        Ok(inner):\n            x = 99\n            match inner:\n                Ok(item):\n                    return *item\n                Err(code):\n                    return code\n        Err(code):\n            return code\n",
    ] {
        refused(
            &format!("{NESTED}fn run() -> U64:\n    var x: U64 = 41\n{statements}{MAIN}"),
            "this value is lent",
        );
    }
    refused(
        &format!(
            "{NESTED}fn take(slot: &mut U64, value: Deep) -> U64:\n    return read(value)\n\nfn run() -> U64:\n    var x: U64 = 41\n    let found = wrap(&x)\n    return take(&mut x, found)\n{MAIN}"
        ),
        "this value is lent",
    );
}

#[test]
fn nested_sums_preserve_parameter_origins_and_end_loans_after_last_use() {
    checked(&format!("{NESTED}fn forward(value: &U64) -> Deep:\n    let held = pass(wrap(value))\n    return held\n\nfn unwrap(value: Deep) -> &U64:\n    match value:\n        Ok(inner):\n            match inner:\n                Ok(item):\n                    return item\n                Err(code):\n                    return unwrap(value)\n        Err(code):\n            return unwrap(value)\n\nfn run() -> U64:\n    var x: U64 = 41\n    let found = pass(wrap(&x))\n    let seen = read(found)\n    x = 99\n    return seen + x\n{MAIN}")).unwrap();
    // The same frame cell is reused in the loop; every iteration's loan ends
    // before the next write, and matching through both sums keeps the origin.
    checked(&format!("{NESTED}fn run() -> U64:\n    var x: U64 = 0\n    var total: U64 = 0\n    while x < 3:\n        let found = wrap(&x)\n        total += read(found)\n        x += 1\n    return total\n{MAIN}")).unwrap();
}

#[test]
fn declared_sums_carry_references_at_arbitrary_sum_depth() {
    let types = r#"type Leaf:
    Seen(&U64)
    Missing

type Branch:
    Inside(Leaf)
    Outside

type Crown:
    Top(Branch)
    Bare

fn wrap(value: &U64) -> Crown:
    return Top(Inside(Seen(value)))

"#;
    refused(
        &format!(
            "{types}fn escape() -> Crown:\n    let x: U64 = 1\n    return Top(Inside(Seen(&x)))\n{MAIN}"
        ),
        "must borrow from a reference parameter",
    );
    refused(
        &format!(
            "{types}fn escape() -> Crown:\n    var x: U64 = 1\n    let held = wrap(&x)\n    x = 2\n    return held\n{MAIN}"
        ),
        "this value is lent",
    );
    checked(&format!(
        "{types}fn forward(value: &U64) -> Crown:\n    return wrap(value)\n{MAIN}"
    ))
    .unwrap();
}

#[test]
fn reference_containment_cannot_be_hidden_in_storage_or_a_borrowed_pointee() {
    for source in [
        "fn store(value: Buffer<Result<Result<&U64, U64>, U64>>):\n    return\n",
        "type Keep:\n    value: Result<Result<&U64, U64>, U64>\n",
        "type Keep<T>:\n    value: T\n\nfn store(value: Keep<Result<Result<&U64, U64>, U64>>):\n    return\n",
        "fn store(value: .{held: Result<Result<&U64, U64>, U64>}):\n    return\n",
        "fn store(value: [Result<Result<&U64, U64>, U64>; 2]):\n    return\n",
        "fn read(value: &Result<Result<&U64, U64>, U64>):\n    return\n",
        "fn write(value: &mut Result<Result<&U64, U64>, U64>):\n    return\n",
        // The Buffer sees an incomplete recursive sum before Seen is built.
        "type Tree:\n    Fork(Buffer<Tree>)\n    Seen(Result<Result<&U64, U64>, U64>)\n",
        "type Tree:\n    Fork(Buffer<Node>)\n    Seen(Result<Result<&U64, U64>, U64>)\n\ntype Node:\n    tree: Tree\n",
    ] {
        assert!(checked(&format!("{source}{MAIN}")).is_err(), "{source}");
    }
    // Value-level address construction also checks containment.
    refused(
        &format!(
            "{NESTED}fn escape(x: &U64):\n    let held = wrap(x)\n    let address = &held\n{MAIN}"
        ),
        "not at a value that holds a reference",
    );
    checked(&format!(
        "type Tree:\n    Fork(Buffer<Tree>)\n    Leaf(U64)\n{MAIN}"
    ))
    .unwrap();
}

#[test]
fn invariant_write_permission_is_required_even_in_the_defining_file() {
    let library = r#"sealed type Shelf:
    data: Buffer<U8>
    length: U64
    capacity: U64
    invariant memory.prefix(data, length, capacity)

fn overwrite(slot: &mut U64):
    *slot = 1

"#;
    // Without written clauses the function's proof is required implicitly (f9c4e93d): the
    // body is checked exactly as under an explicit `ensures true`, and the proof models
    // refuse the unmodeled write before anything is published.
    for body in [
        "fn tamper(value: &mut Shelf):\n    overwrite(&mut (*value).length)\n",
        "fn tamper(value: &mut Shelf):\n    let _ = memory.init(&mut (*value).data, 0, 1)\n",
    ] {
        let prepared = |body: &str| {
            let source = format!("{library}{body}{MAIN}");
            let logical = super::check::program(&super::parse(&source).unwrap()).map_err(|e| e.message.to_string())?;
            super::source_vc::prepare(&logical).map(|_| ()).map_err(|e| format!("{}: {}", e.code, e.message))
        };
        let implicit = prepared(body);
        let refusal = implicit.clone().expect_err(body);
        assert!(refusal.starts_with("W0401: memory proof "), "{refusal}: {body}");
        let explicit = body.replacen(":\n", ":\n    ensures true\n    proof:\n        auto\n", 1);
        assert_eq!(implicit, prepared(&explicit), "{body}");
    }
    // Permission to open the invariant does not grant an unmodeled callee
    // permission to modify it invisibly to the proof interpreter.
    let source = format!(
        "{library}fn tamper(value: &mut Shelf):\n    ensures true\n    proof:\n        auto\n    overwrite(&mut (*value).length)\n{MAIN}"
    );
    let logical = super::check::program(&super::parse(&source).unwrap()).unwrap();
    assert!(super::source_vc::prepare(&logical).is_err());
}

#[test]
fn references_extracted_by_value_still_protect_the_callers_source() {
    let extract = r#"fn extract(value: Deep, fallback: &U64) -> &U64:
    match value:
        Ok(inner):
            match inner:
                Ok(item):
                    return item
                Err(code):
                    return fallback
        Err(code):
            return fallback

"#;
    refused(
        &format!(
            "{NESTED}{extract}fn run() -> U64:\n    var x: U64 = 41\n    let spare: U64 = 7\n    let held = extract(pass(wrap(&x)), &spare)\n    x = 99\n    return *held\n{MAIN}"
        ),
        "this value is lent",
    );
    checked(&format!("{NESTED}{extract}fn run() -> U64:\n    var x: U64 = 41\n    let spare: U64 = 7\n    let held = extract(pass(wrap(&x)), &spare)\n    let seen = *held\n    x = 99\n    return seen + x\n{MAIN}")).unwrap();
    // A reborrow follows the incoming referent, while the scalar carrier of
    // another variant is owned by this invocation and cannot escape.
    checked(&format!(
        "{NESTED}{}{MAIN}",
        extract.replace("return item", "return &*item")
    ))
    .unwrap();
    refused(
        &format!(
            "{NESTED}fn escape(value: Deep) -> &U64:\n    match value:\n        Ok(inner):\n            match inner:\n                Ok(item):\n                    return item\n                Err(code):\n                    return &code\n        Err(code):\n            return &code\n{MAIN}"
        ),
        "must borrow from a reference parameter",
    );
    // The error arm of `?` is an implicit return and must check nested origins.
    refused(
        &format!(
            "fn wrap_error(x: &U64) -> Result<U64, Result<&U64, U64>>:\n    return Err(Ok(x))\n\nfn escape() -> Result<U64, Result<&U64, U64>>:\n    let x: U64 = 41\n    let value = wrap_error(&x)?\n    return Ok(value)\n{MAIN}"
        ),
        "must borrow from a reference parameter",
    );
}

#[test]
fn nested_references_keep_owners_live_until_the_last_use() {
    let library = r#"type View = Result<Result<&Buffer<U64>, U64>, U64>

fn wrap(buffer: &Buffer<U64>) -> View:
    return Ok(Ok(buffer))

fn observe(value: View) -> U64:
    match value:
        Ok(inner):
            match inner:
                Ok(buffer):
                    return memory.capacity(buffer)
                Err(code):
                    return code
        Err(code):
            return code

"#;
    refused(
        &format!(
            "{library}fn close(buffer: Buffer<U64>) -> Result<(), Buffer<U64>>:\n    let held = wrap(&buffer)\n    let closed = memory.try_release(move buffer)\n    let seen = observe(held)\n    return move closed\n{MAIN}"
        ),
        "this value is lent",
    );
    checked(&format!("{library}fn close(buffer: Buffer<U64>) -> Result<(), Buffer<U64>>:\n    let held = wrap(&buffer)\n    let seen = observe(held)\n    return memory.try_release(move buffer)\n{MAIN}")).unwrap();
}

#[test]
fn actual_imported_public_reference_apis_keep_their_proof_obligations() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/references/elements.wy");
    let (source, parts) = super::files::read_bundle(&path).unwrap();
    let syntax = super::parse_modules(&source, &super::files::starts(&parts)).unwrap();
    let logical = super::check::program(&syntax).unwrap();
    let goals = super::source_vc::prepare(&logical).unwrap();
    assert!(!goals.is_empty());
    super::concrete::program(logical).unwrap();
}
