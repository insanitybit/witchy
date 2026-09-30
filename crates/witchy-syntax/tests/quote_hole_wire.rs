use witchy_syntax::compiler_session::quote_hole_wire::{self, Category, Hole, HoleCategory, Plan};

#[test]
fn typed_quote_hole_plan_round_trips_without_source_text() {
    let plan = Plan {
        category: Category::Item,
        template: "session:item:7".into(),
        source_line: 42,
        holes: vec![
            Hole {
                category: HoleCategory::Expr,
                handle: "session:expr:3".into(),
            },
            Hole {
                category: HoleCategory::Type,
                handle: "session:type:4".into(),
            },
            Hole {
                category: HoleCategory::Pattern,
                handle: "session:pattern:5".into(),
            },
        ],
    };
    let encoded = quote_hole_wire::encode(&plan).unwrap();
    assert_eq!(quote_hole_wire::decode(&encoded).unwrap(), plan);
    let mut trailing = encoded.clone();
    trailing.push(b'x');
    assert!(
        quote_hole_wire::decode(&trailing)
            .unwrap_err()
            .contains("trailing")
    );
    let mut wrong_category = encoded.clone();
    let first_hole_tag = 3 + 1 + 4 + 4 + plan.template.len() + 2;
    wrong_category[first_hole_tag] = b'I';
    assert!(
        quote_hole_wire::decode(&wrong_category)
            .unwrap_err()
            .contains("hole category")
    );
    assert!(
        quote_hole_wire::decode(&encoded[..encoded.len() - 1])
            .unwrap_err()
            .contains("truncated")
    );
}

#[test]
fn quote_hole_plan_bounds_are_enforced_before_allocation() {
    let mut plan = Plan {
        category: Category::Expr,
        template: "template".into(),
        source_line: 1,
        holes: vec![Hole {
            category: HoleCategory::Expr,
            handle: "x".into(),
        }],
    };
    plan.holes = vec![plan.holes[0].clone(); 257];
    assert!(
        quote_hole_wire::encode(&plan)
            .unwrap_err()
            .contains("out of range")
    );
    plan.holes.truncate(1);
    plan.holes[0].handle = "x".repeat(4_097);
    assert!(
        quote_hole_wire::encode(&plan)
            .unwrap_err()
            .contains("length")
    );
    plan.holes[0].handle = "x".into();
    plan.source_line = 0;
    assert!(
        quote_hole_wire::encode(&plan)
            .unwrap_err()
            .contains("out of range")
    );
}
