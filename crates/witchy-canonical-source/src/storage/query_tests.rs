use super::*;

fn named(name: &str, arguments: Vec<Type>) -> Type {
    Type::Named(name.into(), arguments)
}

fn unique(inner: Type) -> Type {
    Type::Qualified(TypeQual::Unique, Box::new(inner))
}

fn definitions(source: &str) -> BTreeMap<String, TypeDef> {
    crate::aggregates::type_definitions(
        &witchy_syntax::parser::parse_module(source).expect("storage declarations parse"),
    )
}

#[test]
fn unique_demand_matches_full_containment_projection() {
    let definitions = definitions(
        "type Boxed(a):\n    value: a\n\ntype Phantom(a):\n    Empty\n\ntype Chain(a):\n    Next(Chain(a))\n    Value(a)\n",
    );
    let int = named("Int", vec![]);
    let unique_int = unique(int.clone());
    let cases = [
        (int.clone(), Some(false)),
        (unique_int.clone(), Some(true)),
        (Type::Qualified(TypeQual::Frozen, Box::new(unique_int.clone())), Some(true)),
        (Type::Qualified(TypeQual::View, Box::new(unique_int.clone())), Some(false)),
        (Type::Slice(Box::new(unique_int.clone())), Some(false)),
        (Type::Tuple(vec![int.clone(), unique_int.clone()]), Some(true)),
        (named("List", vec![unique_int.clone()]), Some(true)),
        (named("Boxed", vec![unique_int.clone()]), Some(true)),
        (named("Phantom", vec![unique_int.clone()]), Some(false)),
        (named("Chain", vec![int]), Some(false)),
        (named("Chain", vec![unique_int]), Some(true)),
        (named("List", vec![]), None),
        (named("Boxed", vec![]), None),
    ];
    for (ty, expected) in cases {
        let full = stored_containment(&ty, &definitions, ContainmentDemand::ALL)
            .map(|facts| facts.unique_storage);
        assert_eq!(full, expected, "full containment: {ty:?}");
        assert_eq!(contains_unique_storage(&ty, &definitions), full, "unique demand: {ty:?}");
    }
}

#[test]
fn unique_demand_preserves_growing_storage_refusal_after_a_positive_field() {
    let definitions = definitions(
        "type Grow(a):\n    Value(a)\n    More(Grow(List(a)))\n",
    );
    let grow = named("Grow", vec![named("Int", vec![])]);
    assert_eq!(contains_unique_storage(&grow, &definitions), None);
    // Finding a unique child must not suppress a later unfinished proof.
    let both = Type::Tuple(vec![unique(named("Int", vec![])), grow]);
    assert_eq!(contains_unique_storage(&both, &definitions), None);
}
