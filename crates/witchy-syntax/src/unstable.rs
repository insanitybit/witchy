//! Source-level inventory for unstable Witchy features.
//!
//! This stays in the syntax crate so native commands, the LSP, and browser
//! frontends can agree on feature names, locations, and warning text without
//! turning an unstable feature into a type error.

use crate::lexer::{tokenize, Tok};

/// The canonical warning for the experimental region surface.
pub const REGION_WARNING: &str =
    "`region:` is unstable and may change or be removed; do not rely on its current syntax or performance contract";

/// One source occurrence of an unstable feature. Positions are 1-based and the
/// end position is exclusive, matching lexer tokens and parser diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeatureUse {
    pub line: u32,
    pub column: u32,
    pub end_line: u32,
    pub end_column: u32,
    pub warning: &'static str,
}

/// Find unstable feature spellings in source.
///
/// Token inspection deliberately ignores comments and string contents. A lex
/// failure returns no warnings because the parser owns the authoritative error.
pub fn feature_uses(source: &str) -> Vec<FeatureUse> {
    let Ok(tokens) = tokenize(source) else {
        return Vec::new();
    };
    tokens
        .into_iter()
        .filter_map(|token| {
            matches!(token.kind, Tok::Region).then_some(FeatureUse {
                line: token.line,
                column: token.col,
                end_line: token.end_line,
                end_column: token.end_col,
                warning: REGION_WARNING,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regions_are_reported_but_comments_and_strings_are_not() {
        let source = "// region:\nfn main() -> String:\n    let label = \"region:\"\n    region -> String:\n        region:\n            label\n";
        let uses = feature_uses(source);

        assert_eq!(uses.len(), 2, "{uses:?}");
        assert_eq!((uses[0].line, uses[0].column), (4, 5));
        assert_eq!((uses[1].line, uses[1].column), (5, 9));
        assert!(uses.iter().all(|feature| feature.warning == REGION_WARNING));
    }

    #[test]
    fn malformed_source_leaves_the_parse_error_authoritative() {
        assert!(feature_uses("\"unterminated").is_empty());
    }
}
