//! Lexer for witchy's braced-hybrid surface syntax.
//!
//! Produces a flat token stream with line/column spans so the parser can emit
//! Gleam-quality error messages.

use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    // Literals
    Int(i64),
    Float(f64),
    Str(String),
    Ident(String),

    // Keywords
    Actor,
    Fn,
    Let,
    Var,
    On,
    Match,
    Use,
    Pub,
    If,
    Else,
    True,
    False,
    Spawn,
    Type,
    Inout,
    Sink,
    Import,
    While,
    For,
    In,
    Return,
    Break,
    Continue,
    Update,

    // Grouping / punctuation
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Comma,
    Colon,
    Dot,
    DotDot,
    DotDotEq,
    Underscore,

    // Operators
    Eq,     // =
    EqEq,   // ==
    NotEq,  // !=
    Lt,     // <
    Gt,     // >
    LtEq,   // <=
    GtEq,   // >=
    Plus,    // +
    Minus,   // -
    Star,    // *
    Slash,   // /
    Percent, // %
    Concat, // <>
    Pipe,   // |>
    Bar,    // |  (or-patterns)
    PlusEq,    // +=
    MinusEq,   // -=
    StarEq,    // *=
    SlashEq,   // /=
    PercentEq, // %=
    Amp,    // &  (bitwise and)
    Caret,  // ^  (bitwise xor)
    Tilde,  // ~  (bitwise not)
    Shl,    // << (shift left)
    Shr,    // >> (shift right)
    LArrow, // <-
    RArrow, // ->
    AndAnd, // &&
    OrOr,   // ||
    Bang,   // !
    Question, // ?

    Eof,
}

impl fmt::Display for Tok {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use Tok::*;
        match self {
            Int(n) => write!(f, "{n}"),
            Float(x) => write!(f, "{x}"),
            Str(s) => write!(f, "{s:?}"),
            Ident(s) => write!(f, "{s}"),
            Actor => write!(f, "actor"),
            Fn => write!(f, "fn"),
            Let => write!(f, "let"),
            Var => write!(f, "var"),
            On => write!(f, "on"),
            Match => write!(f, "match"),
            Use => write!(f, "use"),
            Pub => write!(f, "pub"),
            If => write!(f, "if"),
            Else => write!(f, "else"),
            True => write!(f, "true"),
            False => write!(f, "false"),
            Spawn => write!(f, "spawn"),
            Type => write!(f, "type"),
            Inout => write!(f, "inout"),
            Sink => write!(f, "sink"),
            Import => write!(f, "import"),
            While => write!(f, "while"),
            For => write!(f, "for"),
            In => write!(f, "in"),
            Return => write!(f, "return"),
            Break => write!(f, "break"),
            Continue => write!(f, "continue"),
            Update => write!(f, "update"),
            LParen => write!(f, "("),
            RParen => write!(f, ")"),
            LBrace => write!(f, "{{"),
            RBrace => write!(f, "}}"),
            LBracket => write!(f, "["),
            RBracket => write!(f, "]"),
            Comma => write!(f, ","),
            Colon => write!(f, ":"),
            Dot => write!(f, "."),
            DotDot => write!(f, ".."),
            DotDotEq => write!(f, "..="),
            Underscore => write!(f, "_"),
            Eq => write!(f, "="),
            EqEq => write!(f, "=="),
            NotEq => write!(f, "!="),
            Lt => write!(f, "<"),
            Gt => write!(f, ">"),
            LtEq => write!(f, "<="),
            GtEq => write!(f, ">="),
            Plus => write!(f, "+"),
            Minus => write!(f, "-"),
            Star => write!(f, "*"),
            Slash => write!(f, "/"),
            Percent => write!(f, "%"),
            Concat => write!(f, "<>"),
            Pipe => write!(f, "|>"),
            Bar => write!(f, "|"),
            PlusEq => write!(f, "+="),
            MinusEq => write!(f, "-="),
            StarEq => write!(f, "*="),
            SlashEq => write!(f, "/="),
            PercentEq => write!(f, "%="),
            Amp => write!(f, "&"),
            Caret => write!(f, "^"),
            Tilde => write!(f, "~"),
            Shl => write!(f, "<<"),
            Shr => write!(f, ">>"),
            LArrow => write!(f, "<-"),
            RArrow => write!(f, "->"),
            AndAnd => write!(f, "&&"),
            OrOr => write!(f, "||"),
            Bang => write!(f, "!"),
            Question => write!(f, "?"),
            Eof => write!(f, "end of input"),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: Tok,
    pub line: u32,
    pub col: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LexError {
    pub message: String,
    pub line: u32,
    pub col: u32,
}

impl fmt::Display for LexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "lex error at {}:{}: {}", self.line, self.col, self.message)
    }
}

impl std::error::Error for LexError {}

struct Lexer {
    chars: Vec<char>,
    pos: usize,
    line: u32,
    col: u32,
}

impl Lexer {
    fn new(src: &str) -> Self {
        Self {
            chars: src.chars().collect(),
            pos: 0,
            line: 1,
            col: 1,
        }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn peek2(&self) -> Option<char> {
        self.chars.get(self.pos + 1).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.chars.get(self.pos).copied()?;
        self.pos += 1;
        if c == '\n' {
            self.line += 1;
            self.col = 1;
        } else {
            self.col += 1;
        }
        Some(c)
    }

    fn err(&self, message: impl Into<String>) -> LexError {
        LexError {
            message: message.into(),
            line: self.line,
            col: self.col,
        }
    }

    fn tokenize(mut self) -> Result<Vec<Token>, LexError> {
        let mut out = Vec::new();
        loop {
            self.skip_trivia();
            let (line, col) = (self.line, self.col);
            let Some(c) = self.peek() else {
                out.push(Token { kind: Tok::Eof, line, col });
                return Ok(out);
            };
            // A string literal may expand into several tokens (interpolation),
            // so it pushes directly; everything else yields a single token.
            if c == '"' {
                for kind in self.string()? {
                    out.push(Token { kind, line, col });
                }
                continue;
            }
            let kind = match c {
                '0'..='9' => self.number()?,
                'a'..='z' | 'A'..='Z' | '_' => self.ident_or_keyword(),
                _ => self.operator()?,
            };
            out.push(Token { kind, line, col });
        }
    }

    fn skip_trivia(&mut self) {
        loop {
            match self.peek() {
                Some(c) if c.is_whitespace() => {
                    self.bump();
                }
                Some('/') if self.peek2() == Some('/') => {
                    while let Some(c) = self.peek() {
                        if c == '\n' {
                            break;
                        }
                        self.bump();
                    }
                }
                _ => return,
            }
        }
    }

    fn number(&mut self) -> Result<Tok, LexError> {
        // Hex (`0x..`) and binary (`0b..`) integer literals.
        if self.peek() == Some('0') {
            match self.peek2() {
                Some('x') | Some('X') => {
                    self.bump();
                    self.bump();
                    return self.radix_int(16, "hexadecimal");
                }
                Some('b') | Some('B') => {
                    self.bump();
                    self.bump();
                    return self.radix_int(2, "binary");
                }
                _ => {}
            }
        }
        let mut text = String::new();
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() || c == '_' {
                if c != '_' {
                    text.push(c);
                }
                self.bump();
            } else {
                break;
            }
        }
        // Fractional part: only if a digit follows the dot (so `x.0` field-ish
        // cases and `1.method` stay unambiguous — methods come later).
        if self.peek() == Some('.') && self.peek2().is_some_and(|c| c.is_ascii_digit()) {
            text.push('.');
            self.bump();
            while let Some(c) = self.peek() {
                if c.is_ascii_digit() {
                    text.push(c);
                    self.bump();
                } else {
                    break;
                }
            }
            let value = text
                .parse::<f64>()
                .map_err(|_| self.err(format!("invalid float literal `{text}`")))?;
            return Ok(Tok::Float(value));
        }
        let value = text
            .parse::<i64>()
            .map_err(|_| self.err(format!("invalid integer literal `{text}`")))?;
        Ok(Tok::Int(value))
    }

    /// Lex the digits of a `0x`/`0b` literal (the prefix already consumed),
    /// allowing `_` separators. Errors on no digits or an out-of-range value.
    fn radix_int(&mut self, radix: u32, name: &str) -> Result<Tok, LexError> {
        let mut text = String::new();
        while let Some(c) = self.peek() {
            if c == '_' {
                self.bump();
            } else if c.is_digit(radix) {
                text.push(c);
                self.bump();
            } else {
                break;
            }
        }
        if text.is_empty() {
            return Err(self.err(format!("{name} literal has no digits")));
        }
        let value = i64::from_str_radix(&text, radix)
            .map_err(|_| self.err(format!("invalid {name} integer literal `{text}`")))?;
        Ok(Tok::Int(value))
    }

    fn ident_or_keyword(&mut self) -> Tok {
        let mut text = String::new();
        while let Some(c) = self.peek() {
            if c.is_ascii_alphanumeric() || c == '_' {
                text.push(c);
                self.bump();
            } else {
                break;
            }
        }
        match text.as_str() {
            "actor" => Tok::Actor,
            "fn" => Tok::Fn,
            "let" => Tok::Let,
            "var" => Tok::Var,
            "on" => Tok::On,
            "match" => Tok::Match,
            "use" => Tok::Use,
            "pub" => Tok::Pub,
            "if" => Tok::If,
            "else" => Tok::Else,
            "true" => Tok::True,
            "false" => Tok::False,
            "spawn" => Tok::Spawn,
            "type" => Tok::Type,
            "inout" => Tok::Inout,
            "sink" => Tok::Sink,
            "import" => Tok::Import,
            "while" => Tok::While,
            "for" => Tok::For,
            "in" => Tok::In,
            "return" => Tok::Return,
            "break" => Tok::Break,
            "continue" => Tok::Continue,
            "update" => Tok::Update,
            "_" => Tok::Underscore,
            _ => Tok::Ident(text),
        }
    }

    /// Lex a string literal. A plain string is a single `Str` token. A string
    /// with `${ expr }` interpolations expands to the token stream for
    /// `( lit0 <> to_string(expr0) <> lit1 <> ... )`, so the parser needs no
    /// special handling and interpolation works in both backends (`to_string` +
    /// concat). Write `\$` for a literal `$`.
    fn string(&mut self) -> Result<Vec<Tok>, LexError> {
        self.bump(); // opening quote
        let mut text = String::new();
        let mut out: Vec<Tok> = Vec::new();
        let mut interpolated = false;
        loop {
            match self.bump() {
                None => return Err(self.err("unterminated string literal")),
                Some('"') => break,
                Some('\\') => {
                    let esc = self
                        .bump()
                        .ok_or_else(|| self.err("unterminated escape in string literal"))?;
                    match esc {
                        'n' => text.push('\n'),
                        't' => text.push('\t'),
                        'r' => text.push('\r'),
                        '0' => text.push('\0'),
                        '\\' => text.push('\\'),
                        '"' => text.push('"'),
                        '$' => text.push('$'),
                        other => {
                            return Err(self.err(format!("unknown escape `\\{other}`")));
                        }
                    }
                }
                Some('$') if self.peek() == Some('{') => {
                    self.bump(); // consume '{'
                    // Close off the preceding literal segment.
                    if interpolated {
                        out.push(Tok::Concat);
                    } else {
                        out.push(Tok::LParen);
                        interpolated = true;
                    }
                    out.push(Tok::Str(std::mem::take(&mut text)));
                    // `<> to_string( <embedded expression> )`
                    let src = self.interp_source()?;
                    let expr_toks = Lexer::new(&src).tokenize()?;
                    out.push(Tok::Concat);
                    out.push(Tok::Ident("to_string".into()));
                    out.push(Tok::LParen);
                    for t in expr_toks {
                        if t.kind == Tok::Eof {
                            break;
                        }
                        out.push(t.kind);
                    }
                    out.push(Tok::RParen);
                }
                Some(c) => text.push(c),
            }
        }
        if interpolated {
            out.push(Tok::Concat);
            out.push(Tok::Str(text));
            out.push(Tok::RParen);
            Ok(out)
        } else {
            Ok(vec![Tok::Str(text)])
        }
    }

    /// Read the source of a `${ ... }` interpolation (the opening `${` already
    /// consumed) up to the matching `}`. Tracks brace depth and skips over nested
    /// string literals so their braces and quotes don't confuse the match.
    fn interp_source(&mut self) -> Result<String, LexError> {
        let mut depth = 1;
        let mut src = String::new();
        loop {
            match self.bump() {
                None => return Err(self.err("unterminated `${` interpolation")),
                Some('{') => {
                    depth += 1;
                    src.push('{');
                }
                Some('}') => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(src);
                    }
                    src.push('}');
                }
                Some('"') => {
                    src.push('"');
                    loop {
                        match self.bump() {
                            None => return Err(self.err("unterminated string in interpolation")),
                            Some('\\') => {
                                src.push('\\');
                                if let Some(c) = self.bump() {
                                    src.push(c);
                                }
                            }
                            Some('"') => {
                                src.push('"');
                                break;
                            }
                            Some(c) => src.push(c),
                        }
                    }
                }
                Some(c) => src.push(c),
            }
        }
    }

    fn operator(&mut self) -> Result<Tok, LexError> {
        let c = self.bump().expect("operator() called at EOF");
        let two = self.peek();
        let tok = match (c, two) {
            ('=', Some('=')) => {
                self.bump();
                Tok::EqEq
            }
            ('!', Some('=')) => {
                self.bump();
                Tok::NotEq
            }
            ('<', Some('=')) => {
                self.bump();
                Tok::LtEq
            }
            ('>', Some('=')) => {
                self.bump();
                Tok::GtEq
            }
            ('<', Some('>')) => {
                self.bump();
                Tok::Concat
            }
            ('<', Some('-')) => {
                self.bump();
                Tok::LArrow
            }
            ('<', Some('<')) => {
                self.bump();
                Tok::Shl
            }
            ('>', Some('>')) => {
                self.bump();
                Tok::Shr
            }
            ('-', Some('>')) => {
                self.bump();
                Tok::RArrow
            }
            ('+', Some('=')) => {
                self.bump();
                Tok::PlusEq
            }
            ('-', Some('=')) => {
                self.bump();
                Tok::MinusEq
            }
            ('*', Some('=')) => {
                self.bump();
                Tok::StarEq
            }
            ('/', Some('=')) => {
                self.bump();
                Tok::SlashEq
            }
            ('%', Some('=')) => {
                self.bump();
                Tok::PercentEq
            }
            ('|', Some('>')) => {
                self.bump();
                Tok::Pipe
            }
            ('&', Some('&')) => {
                self.bump();
                Tok::AndAnd
            }
            ('|', Some('|')) => {
                self.bump();
                Tok::OrOr
            }
            ('|', _) => Tok::Bar,
            ('&', _) => Tok::Amp,
            ('^', _) => Tok::Caret,
            ('~', _) => Tok::Tilde,
            ('!', _) => Tok::Bang,
            ('=', _) => Tok::Eq,
            ('<', _) => Tok::Lt,
            ('>', _) => Tok::Gt,
            ('+', _) => Tok::Plus,
            ('-', _) => Tok::Minus,
            ('*', _) => Tok::Star,
            ('/', _) => Tok::Slash,
            ('%', _) => Tok::Percent,
            ('?', _) => Tok::Question,
            ('(', _) => Tok::LParen,
            (')', _) => Tok::RParen,
            ('{', _) => Tok::LBrace,
            ('}', _) => Tok::RBrace,
            ('[', _) => Tok::LBracket,
            (']', _) => Tok::RBracket,
            (',', _) => Tok::Comma,
            (':', _) => Tok::Colon,
            ('.', Some('.')) => {
                self.bump();
                if self.peek() == Some('=') {
                    self.bump();
                    Tok::DotDotEq
                } else {
                    Tok::DotDot
                }
            }
            ('.', _) => Tok::Dot,
            (other, _) => return Err(self.err(format!("unexpected character `{other}`"))),
        };
        Ok(tok)
    }
}

/// Tokenize witchy source into a stream ending in `Tok::Eof`.
pub fn tokenize(src: &str) -> Result<Vec<Token>, LexError> {
    Lexer::new(src).tokenize()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<Tok> {
        tokenize(src).unwrap().into_iter().map(|t| t.kind).collect()
    }

    #[test]
    fn lexes_a_small_program() {
        let src = r#"
            // a greeter
            fn greet(name: String) -> String {
              "hi, " <> name
            }
        "#;
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
                Tok::LBrace,
                Tok::Str("hi, ".into()),
                Tok::Concat,
                Tok::Ident("name".into()),
                Tok::RBrace,
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
        // "a${x}b" lexes to the token stream for `("a" <> to_string(x) <> "b")`.
        assert_eq!(
            kinds(r#""a${x}b""#),
            vec![
                Tok::LParen,
                Tok::Str("a".into()),
                Tok::Concat,
                Tok::Ident("to_string".into()),
                Tok::LParen,
                Tok::Ident("x".into()),
                Tok::RParen,
                Tok::Concat,
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
}
