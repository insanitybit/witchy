//! Brands (docs/design-brands.md): what the instances made for them are.
fn instances(source: &str, name: &str) -> usize {
    let syntax = crate::compiler::parse(source).expect("parses");
    let logical = super::check::program(&syntax).expect("checks");
    let program = super::concrete::program(logical).expect("lowers");
    program.names.iter().filter(|n| n.as_str() == name || n.starts_with(&format!("{name}<"))).count()
}
/// Erased at any depth: a brand inside a tuple, given to a generic function from two openings,
/// makes one instance (review: the anonymous record kept its brand, and there were two).
#[test]
fn a_brand_inside_a_tuple_is_erased() {
    let source = "type Row<G>:\n    n: U64\n\nfn issue<G: Fresh>() -> Row<G>:\n    return Row(2)\n\nfn keep<T>(v: T) -> T:\n    return v\n\nfn main():\n    brand issue() as a:\n        let v = keep((a, true))\n    brand issue() as b:\n        let w = keep((b, true))\n";
    assert_eq!(instances(source, "keep"), 1);
    // The same through a declared generic record and an anonymous sum.
    let wrapped = "type Row<G>:\n    n: U64\n\ntype Box<T>:\n    item: T\n\nfn issue<G: Fresh>() -> Row<G>:\n    return Row(2)\n\nfn keep<T>(v: T) -> T:\n    return v\n\nfn main():\n    brand issue() as a:\n        let v = keep(Box((a, 1)))\n    brand issue() as b:\n        let w = keep(Box((b, 1)))\n";
    assert_eq!(instances(wrapped, "keep"), 1);
}

#[test]
fn opening_requires_the_brand_keyword_and_as_binder() {
    let source = "fn main():\n    brand issue() as row:\n        ()\n";
    assert!(crate::compiler::parse(source).is_ok());
    assert!(crate::compiler::parse(&source.replace("brand", "with")).is_err());
    assert!(crate::compiler::parse(&source.replace(" as row", "")).is_err());
}
