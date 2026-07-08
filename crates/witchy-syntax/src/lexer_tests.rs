    use super::*;

    fn kinds(src: &str) -> Vec<Tok> {
        tokenize(src).unwrap().into_iter().map(|t| t.kind).collect()
    }

    #[test]
    fn rejects_tab_in_leading_indentation() {
        // BUG-246: a tab counts as one column, so a tab-indented line under a
        // space-indented block would lex shallower than it looks and silently
        // escape the block. Reject it with a clear lex error instead.
        let src = "fn main(console: Console):\n    if false:\n\tprint(console, \"x\")\n";
        let err = tokenize(src).expect_err("a tab-indented line must be rejected");
        assert!(err.message.contains("tab in leading indentation"), "{}", err.message);
        assert_eq!(err.line, 3, "error points at the tabbed line");
        // A tab between tokens (not leading indentation) is fine.
        assert!(tokenize("fn f() -> Int:\n    1\t+ 2\n").is_ok());
        // A stray tab on an otherwise-blank line is harmless.
        assert!(tokenize("fn f() -> Int:\n    1\n\t\n").is_ok());
    }

    #[test]
    fn captures_own_line_comments_not_trailing() {
        let src = "// header\nfn f() -> Int:\n    // inner\n    5 // trailing\n/* block */\n";
        let cs = own_line_comments(src);
        assert_eq!(
            cs,
            vec![
                (1, 1, "// header".to_string()),
                (3, 5, "// inner".to_string()),
                (5, 1, "/* block */".to_string()),
            ]
        );
    }

    #[test]
    fn skips_block_comments_including_nested() {
        // Block comments (nesting) are trivia; division still lexes outside them.
        assert_eq!(
            kinds("a /* x */ /* /* y */ z */ b"),
            vec![Tok::Ident("a".into()), Tok::Ident("b".into()), Tok::Eof]
        );
        assert_eq!(
            kinds("8 / 2"),
            vec![Tok::Int(8), Tok::Slash, Tok::Int(2), Tok::Eof]
        );
    }

    #[test]
    fn lexes_a_small_program() {
        let src = "fn greet(name: String) -> String: \"hi, \" + name";
        let toks = kinds(src);
        assert_eq!(
            toks,
            vec![
                Tok::Fn,
                Tok::Ident("greet".into()),
                Tok::LParen,
                Tok::Ident("name".into()),
                Tok::Colon,
                Tok::Ident("String".into()),
                Tok::RParen,
                Tok::RArrow,
                Tok::Ident("String".into()),
                Tok::Colon,
                Tok::Str("hi, ".into()),
                Tok::Plus,
                Tok::Ident("name".into()),
                Tok::Eof,
            ]
        );
    }

    #[test]
    fn lexes_operators_and_literals() {
        assert_eq!(
            kinds("x |> f(1, 2.5) <- _"),
            vec![
                Tok::Ident("x".into()),
                Tok::Pipe,
                Tok::Ident("f".into()),
                Tok::LParen,
                Tok::Int(1),
                Tok::Comma,
                Tok::Float(2.5),
                Tok::RParen,
                Tok::LArrow,
                Tok::Underscore,
                Tok::Eof,
            ]
        );
    }

    #[test]
    fn tracks_line_and_col() {
        let toks = tokenize("a\n  b").unwrap();
        assert_eq!((toks[0].line, toks[0].col), (1, 1));
        assert_eq!((toks[1].line, toks[1].col), (2, 3));
    }

    #[test]
    fn rejects_unterminated_string() {
        assert!(tokenize("\"oops").is_err());
    }

    #[test]
    fn interpolation_expands_to_concat_tokens() {
        // "a${x}b" lexes to the token stream for `("a" + __render(x) + "b")`.
        assert_eq!(
            kinds(r#""a${x}b""#),
            vec![
                Tok::LParen,
                Tok::Str("a".into()),
                Tok::Plus,
                Tok::Ident("__render".into()),
                Tok::LParen,
                Tok::Ident("x".into()),
                Tok::InterpRBrace,
                Tok::Plus,
                Tok::Str("b".into()),
                Tok::RParen,
                Tok::Eof,
            ]
        );
        // A plain string stays a single token (backward compatible).
        assert_eq!(kinds(r#""plain""#), vec![Tok::Str("plain".into()), Tok::Eof]);
        // `\$` is a literal dollar, not an interpolation.
        assert_eq!(kinds(r#""\${x}""#), vec![Tok::Str("${x}".into()), Tok::Eof]);
    }

    #[test]
    fn interpolation_tokens_keep_hole_spans() {
        let toks = tokenize("fn f():\n    \"pre ${value + } post\"\n").unwrap();
        let plus = toks
            .iter()
            .find(|t| t.kind == Tok::Plus && t.line == 2 && t.col == 18)
            .expect("operator inside interpolation keeps source span");
        assert_eq!((plus.line, plus.col), (2, 18));
        let close = toks
            .iter()
            .find(|t| t.kind == Tok::InterpRBrace)
            .expect("interpolation close token is synthetic");
        assert_eq!((close.line, close.col), (2, 20));
    }

    #[test]
    fn underscore_vs_identifier() {
        assert_eq!(kinds("_ _foo"), vec![Tok::Underscore, Tok::Ident("_foo".into()), Tok::Eof]);
    }

    #[test]
    fn compound_assign_tokens() {
        assert_eq!(
            kinds("x += 1"),
            vec![Tok::Ident("x".into()), Tok::PlusEq, Tok::Int(1), Tok::Eof]
        );
        // `-=` is distinct from `->` and a bare `-`.
        assert_eq!(
            kinds("x -= y"),
            vec![Tok::Ident("x".into()), Tok::MinusEq, Tok::Ident("y".into()), Tok::Eof]
        );
    }

    #[test]
    fn bitwise_op_tokens() {
        let id = |s: &str| Tok::Ident(s.into());
        assert_eq!(kinds("a & b"), vec![id("a"), Tok::Amp, id("b"), Tok::Eof]);
        assert_eq!(kinds("a && b"), vec![id("a"), Tok::AndAnd, id("b"), Tok::Eof]);
        assert_eq!(kinds("a ^ b"), vec![id("a"), Tok::Caret, id("b"), Tok::Eof]);
        assert_eq!(kinds("a << b"), vec![id("a"), Tok::Shl, id("b"), Tok::Eof]);
        assert_eq!(kinds("a >> b"), vec![id("a"), Tok::Shr, id("b"), Tok::Eof]);
        assert_eq!(kinds("~a"), vec![Tok::Tilde, id("a"), Tok::Eof]);
    }

    fn laid_out(src: &str) -> Vec<Tok> {
        apply_layout(tokenize(src).unwrap())
            .into_iter()
            .map(|t| t.kind)
            .collect()
    }

    #[test]
    fn layout_inserts_virtual_braces() {
        let id = |s: &str| Tok::Ident(s.into());
        assert_eq!(
            laid_out("fn f():\n    x\n"),
            vec![
                Tok::Fn,
                id("f"),
                Tok::LParen,
                Tok::RParen,
                Tok::LBrace,
                id("x"),
                Tok::RBrace,
                Tok::Eof
            ]
        );
    }

    #[test]
    fn braces_are_rejected() {
        // Braces are no longer part of witchy syntax; blocks come only from the
        // off-side rule, so a literal `{`/`}` is a lex error.
        assert!(tokenize("fn f() { 0 }").is_err());
        assert!(tokenize("fn f():\n    0\n").is_ok());
    }

    #[test]
    fn layout_closes_nested_blocks_on_dedent() {
        // `if` nested in `fn`, both closed by the dedent to the next item.
        let id = |s: &str| Tok::Ident(s.into());
        assert_eq!(
            laid_out("fn f():\n    if a:\n        x\n    y\n"),
            vec![
                Tok::Fn, id("f"), Tok::LParen, Tok::RParen, Tok::LBrace,
                Tok::If, id("a"), Tok::LBrace,
                id("x"),
                Tok::RBrace,
                id("y"),
                Tok::RBrace,
                Tok::Eof
            ]
        );
    }

    #[test]
    fn hex_and_binary_int_literals() {
        assert_eq!(kinds("0xFF"), vec![Tok::Int(255), Tok::Eof]);
        assert_eq!(kinds("0b1010"), vec![Tok::Int(10), Tok::Eof]);
        assert_eq!(kinds("0xff_ff"), vec![Tok::Int(65535), Tok::Eof]);
        // A bare 0 (no x/b) stays a normal decimal literal.
        assert_eq!(kinds("0 + 1"), vec![Tok::Int(0), Tok::Plus, Tok::Int(1), Tok::Eof]);
    }

    #[test]
    fn bar_distinct_from_pipe_and_oror() {
        // `|` (or-patterns) vs `|>` (pipe) vs `||` (logical or).
        assert_eq!(kinds("a | b"), vec![Tok::Ident("a".into()), Tok::Bar, Tok::Ident("b".into()), Tok::Eof]);
        assert_eq!(kinds("a |> b"), vec![Tok::Ident("a".into()), Tok::Pipe, Tok::Ident("b".into()), Tok::Eof]);
        assert_eq!(kinds("a || b"), vec![Tok::Ident("a".into()), Tok::OrOr, Tok::Ident("b".into()), Tok::Eof]);
    }
