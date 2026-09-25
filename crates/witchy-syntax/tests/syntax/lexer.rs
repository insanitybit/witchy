use witchy_syntax::parser::parse_module as tokenize;

    #[test]
    fn rejects_tab_in_leading_indentation() {
        // BUG-246: a tab counts as one column, so a tab-indented line under a
        // space-indented block would lex shallower than it looks and silently
        // escape the block. Reject it with a clear lex error instead.
        let src = "fn main(console: Console):\n    if false:\n\tconsole.print(\"x\")\n";
        let err = tokenize(src).expect_err("a tab-indented line must be rejected");
        assert!(err.message.contains("tab in leading indentation"), "{}", err.message);
        assert_eq!(err.line, 3, "error points at the tabbed line");
        // A tab between tokens (not leading indentation) is fine.
        assert!(tokenize("fn f() -> Int:\n    1\t+ 2\n").is_ok());
        // A stray tab on an otherwise-blank line is harmless.
        assert!(tokenize("fn f() -> Int:\n    1\n\t\n").is_ok());
    }

    #[test]
    fn rejects_unterminated_string() {
        assert!(tokenize("\"oops").is_err());
    }

    #[test]
    fn braces_are_rejected() {
        // Braces are no longer part of witchy syntax; blocks come only from the
        // off-side rule, so a literal `{`/`}` is a lex error.
        assert!(tokenize("fn f() { 0 }").is_err());
        assert!(tokenize("fn f():\n    0\n").is_ok());
    }

