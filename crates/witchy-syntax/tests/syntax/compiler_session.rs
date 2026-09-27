use witchy_syntax::ast::*;
use witchy_syntax::compiler_session::*;
use witchy_syntax::origin::SyntaxCategory;

#[test]
fn session_moves_typed_payloads_without_changing_source_or_crossing_categories() {
    let mut module = witchy_syntax::parser::parse_module("fn main() -> Int:\n    42\n").unwrap();
    let original = module.clone();
    let item = module.items[0].clone();
    let Item::Function(function) = &item else { panic!("function") };
    let block = function.body.clone();
    module.compiler_item_syntax.push(CompilerItemSyntax {
        handle: "item".into(), item: item.clone(), definition_line: 1,
    });
    for value in [7, 42] {
        module.compiler_expr_syntax.push(CompilerExprSyntax {
            handle: "expr".into(), expr: Expr::Int(value), definition_line: 2,
        });
    }
    module.compiler_type_syntax.push(CompilerTypeSyntax {
        handle: "ty".into(), ty: Type::Named("Int".into(), vec![]), definition_line: 3, runtime_identity: false,
    });
    module.compiler_pattern_syntax.push(CompilerPatternSyntax {
        handle: "pat".into(), pattern: Pattern::Wildcard, definition_line: 4,
    });
    module.compiler_stmt_syntax.push(CompilerStmtSyntax {
        handle: "stmt".into(), stmt: Stmt::Return(None), definition_line: 5,
    });
    module.compiler_block_syntax.push(CompilerBlockSyntax {
        handle: "block".into(), block: block.clone(), definition_line: 6,
    });
    let mut session = CompilerSyntaxSession::take_module_payloads(&mut module);
    assert_eq!(module, original);
    assert_eq!(session.items().get("item"), Some(&item));
    assert_eq!(session.expressions().get("expr"), Some(&Expr::Int(42)));
    assert_eq!(session.types().get("ty"), Some(&Type::Named("Int".into(), vec![])));
    assert_eq!(session.patterns().get("pat"), Some(&Pattern::Wildcard));
    assert_eq!(session.statements().get("stmt"), Some(&Stmt::Return(None)));
    assert_eq!(session.blocks().get("block"), Some(&block));
    assert!(session.types().get("expr").is_none());
    assert!(session.expressions().get("ty").is_none());
    assert!(session.expressions().get("missing").is_none());
    // Payload loading has never fabricated expression origin records; the
    // quote/build adapter supplies those at the actual invocation boundary.
    assert!(session.expression_origins().is_empty());
    let handle = session.next_syntax_handle("expression").unwrap();
    assert_eq!(handle, "\0compiler-syntax-instance\0expression\00");
    session.insert_expression(handle.clone(), Expr::Int(99));
    let origin = ComptimeSyntaxOrigin {
        definition_line: 12,
        hole_ancestry: vec![ComptimeHoleOrigin {
            category: SyntaxCategory::Expr, definition_line: 8, invocation_line: 20,
        }],
    };
    session.insert_expression_origin(handle.clone(), origin.clone());
    assert_eq!(session.expressions().get(&handle), Some(&Expr::Int(99)));
    assert_eq!(session.expression_origins().get(&handle), Some(&origin));
    assert!(session.types().get(&handle).is_none());
    assert!(CompilerSyntaxSession::default().expressions().get(&handle).is_none());
}

#[test]
fn invocation_names_and_emissions_preserve_scope_order_and_fresh_entry_state() {
    let mut first = CompilerSyntaxSession::default();
    assert_eq!(first.next_fresh_ident("x").unwrap_err(),
        "meta.fresh is available only during compile-time expansion");
    first.set_invocation(Some("a/b".into()), Some(vec!["helpers".into()]),
        Some(vec!["__witchy_hole_0".into()]));
    assert_eq!(first.next_fresh_ident("x").unwrap(), "__witchy_fresh_612f62_0_x");
    assert_eq!(first.next_fresh_ident("x").unwrap(), "__witchy_fresh_612f62_1_x");
    assert_eq!(first.expr_qualifiers().as_deref(), Some(["helpers".to_string()].as_slice()));
    assert_eq!(first.tag_hole_markers().as_deref(), Some(["__witchy_hole_0".to_string()].as_slice()));
    let mut second = CompilerSyntaxSession::default();
    second.set_invocation(Some("a/b".into()), None, None);
    assert_eq!(second.next_fresh_ident("x").unwrap(), "__witchy_fresh_612f62_0_x");
    assert!(second.expr_qualifiers().is_none());
    assert!(second.tag_hole_markers().is_none());
    assert_eq!(first.next_syntax_handle("expr").unwrap(), "\0compiler-syntax-instance\0expr\00");
    assert_eq!(first.next_syntax_handle("type").unwrap(), "\0compiler-syntax-instance\0type\01");
    let module = witchy_syntax::parser::parse_module("fn emitted() -> Int:\n    42\n").unwrap();
    let item = PositionedComptimeItem {
        output_position: 3,
        emission: ComptimeItemEmission::ModuleSyntax {
            module: Box::new(module), definition_line: 7, hole_ancestry: vec![],
        },
    };
    first.emit_item(item.clone());
    first.emit_expression(ComptimeExprEmission::Syntax(Box::new(Expr::Int(1))));
    first.emit_expression(ComptimeExprEmission::Syntax(Box::new(Expr::Int(2))));
    let (items, expressions) = first.into_emissions();
    assert_eq!(items, vec![item]);
    assert_eq!(expressions, vec![ComptimeExprEmission::Syntax(Box::new(Expr::Int(1))),
        ComptimeExprEmission::Syntax(Box::new(Expr::Int(2)))]);
    assert_eq!(second.into_emissions(), (vec![], vec![]));
}

#[test]
fn explicit_deterministic_budget_accounts_exact_units_and_refuses_without_overflow() {
    let mut budget = DeterministicBudget::new(3);
    assert_eq!(budget.charge(2), Ok(()));
    assert_eq!(budget.remaining(), 1);
    assert_eq!(budget.charge(2), Err(BudgetExceeded));
    assert_eq!(budget.remaining(), 1);
    assert_eq!(budget.charge(1), Ok(()));
    assert_eq!(budget.charge(0), Ok(()));
    assert_eq!(budget.charge(1), Err(BudgetExceeded));
    let mut maximum = DeterministicBudget::new(u64::MAX);
    assert_eq!(maximum.charge(u64::MAX), Ok(()));
    assert_eq!(maximum.remaining(), 0);
    assert_eq!(maximum.charge(u64::MAX), Err(BudgetExceeded));
}

#[test]
fn compiler_contract_and_nonallocating_fresh_probe_preserve_exact_namespace() {
    let contract = CompilerSessionContract::for_source([7; 32], "a/b".into()).unwrap();
    let encoded = contract.encode();
    assert_eq!(CompilerSessionContract::decode(&encoded), Some(contract.clone()));
    assert_eq!(contract.source_digest(), [7; 32]);
    assert_eq!(contract.scope(), "a/b");
    for length in 0..encoded.len() {
        assert!(CompilerSessionContract::decode(&encoded[..length]).is_none());
    }
    let mut trailing = encoded;
    trailing.push(0);
    assert!(CompilerSessionContract::decode(&trailing).is_none());
    let mut invalid_utf8 = contract.encode();
    invalid_utf8[38] = 255;
    assert!(CompilerSessionContract::decode(&invalid_utf8).is_none());
    let mut oversized = contract.encode();
    oversized[34..38].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(CompilerSessionContract::decode(&oversized).is_none());
    assert!(CompilerSessionContract::for_source([7; 32], String::new()).is_err());
    assert!(CompilerSessionContract::for_source([7; 32], "x".repeat(4097)).is_err());
    let mut session = CompilerSyntaxSession::default();
    session.set_invocation(Some("scope/é".into()), None, None);
    for ordinal in 0..101 {
        let expected = format!("__witchy_fresh_73636f70652fc3a9_{ordinal}_名");
        assert_eq!(session.fresh_ident_byte_len("名".len()).unwrap(), expected.len());
        assert_eq!(session.preview_fresh_ident("名").unwrap(), expected);
        assert_eq!(session.next_fresh_ident("名").unwrap(), expected);
    }
    assert!(session.fresh_ident_byte_len(usize::MAX).is_err());
}
