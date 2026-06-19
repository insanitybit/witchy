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
    /// A duration literal like `30s` or `2hr`, carried as whole milliseconds.
    Duration(i64),
    Str(String),
    Ident(String),

    // Keywords
    Fn,
    Gen,
    Yield,
    Async,
    Await,
    Let,
    Var,
    Match,
    Pub,
    If,
    Else,
    True,
    False,
    Type,
    Own,
    Move,
    Import,
    While,
    For,
    In,
    Return,
    Break,
    Continue,
    Trait,
    Impl,
    Where,
    As,
    Retain,
    Without,
    Region,
    Comptime,

    // Grouping / punctuation
    LParen,
    RParen,
    LBrace,
    RBrace,
    /// `.{` — opens an anonymous struct `.{ field: expr, … }`. The only place a
    /// brace is allowed in source (bare `{`/`}` are not witchy syntax).
    DotLBrace,
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
            Duration(ms) => write!(f, "{ms}ms"),
            Str(s) => write!(f, "{s:?}"),
            Ident(s) => write!(f, "{s}"),
            Fn => write!(f, "fn"),
            Gen => write!(f, "gen"),
            Yield => write!(f, "yield"),
            Async => write!(f, "async"),
            Await => write!(f, "await"),
            Let => write!(f, "let"),
            Var => write!(f, "var"),
            Match => write!(f, "match"),
            Pub => write!(f, "pub"),
            If => write!(f, "if"),
            Else => write!(f, "else"),
            True => write!(f, "true"),
            False => write!(f, "false"),
            Type => write!(f, "type"),
            Own => write!(f, "own"),
            Move => write!(f, "move"),
            Import => write!(f, "import"),
            While => write!(f, "while"),
            For => write!(f, "for"),
            In => write!(f, "in"),
            Return => write!(f, "return"),
            Break => write!(f, "break"),
            Continue => write!(f, "continue"),
            Trait => write!(f, "trait"),
            Impl => write!(f, "impl"),
            Where => write!(f, "where"),
            As => write!(f, "as"),
            Retain => write!(f, "retain"),
            Without => write!(f, "without"),
            Region => write!(f, "region"),
            Comptime => write!(f, "comptime"),
            LParen => write!(f, "("),
            RParen => write!(f, ")"),
            LBrace => write!(f, "{{"),
            RBrace => write!(f, "}}"),
            DotLBrace => write!(f, ".{{"),
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
    /// Own-line comments (only whitespace precedes them on their line), captured
    /// as `(line, text)` so the formatter can reproduce them. Trailing comments
    /// on a code line are not captured.
    comments: Vec<(u32, String)>,
}

impl Lexer {
    fn new(src: &str) -> Self {
        Self {
            chars: src.chars().collect(),
            pos: 0,
            line: 1,
            col: 1,
            comments: Vec::new(),
        }
    }

    /// True if only whitespace precedes the current position on this line.
    fn at_line_start(&self) -> bool {
        let mut i = self.pos;
        while i > 0 {
            match self.chars[i - 1] {
                '\n' => return true,
                c if c.is_whitespace() => i -= 1,
                _ => return false,
            }
        }
        true
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

    fn tokenize(&mut self) -> Result<Vec<Token>, LexError> {
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
                for kind in self.string(false)? {
                    out.push(Token { kind, line, col });
                }
                continue;
            }
            // `f"..."` is an f-string: `{expr}` interpolates (Python style),
            // `{{`/`}}` are literal braces.
            if c == 'f' && self.peek2() == Some('"') {
                self.bump(); // consume the `f`
                for kind in self.string(true)? {
                    out.push(Token { kind, line, col });
                }
                continue;
            }
            let kind = match c {
                // Digits directly after a `.` are a tuple element index
                // (`pair.0`, `nested.0.1`), never a number with a fractional
                // part — so `.0.1` lexes as two indices, not the float `0.1`.
                '0'..='9' if matches!(out.last().map(|t| &t.kind), Some(Tok::Dot)) => {
                    self.integer_index()?
                }
                '0'..='9' => self.number()?,
                'a'..='z' | 'A'..='Z' | '_' => self.ident_or_keyword(),
                _ => self.operator()?,
            };
            out.push(Token { kind, line, col });
        }
    }

    /// Record an own-line comment spanning `start..self.pos` at `line`.
    fn record_comment(&mut self, own_line: bool, line: u32, start: usize) {
        if own_line {
            let text: String = self.chars[start..self.pos].iter().collect();
            self.comments.push((line, text.trim_end().to_string()));
        }
    }

    fn skip_trivia(&mut self) {
        loop {
            match self.peek() {
                Some(c) if c.is_whitespace() => {
                    self.bump();
                }
                Some('/') if self.peek2() == Some('/') => {
                    let own_line = self.at_line_start();
                    let (line, start) = (self.line, self.pos);
                    while let Some(c) = self.peek() {
                        if c == '\n' {
                            break;
                        }
                        self.bump();
                    }
                    self.record_comment(own_line, line, start);
                }
                // Block comments `/* ... */`, which nest (so a block containing a
                // block comment can itself be commented out). An unterminated
                // block comment runs to end of input.
                Some('/') if self.peek2() == Some('*') => {
                    let own_line = self.at_line_start();
                    let (line, start) = (self.line, self.pos);
                    self.bump();
                    self.bump();
                    let mut depth = 1u32;
                    while depth > 0 {
                        match (self.peek(), self.peek2()) {
                            (Some('/'), Some('*')) => {
                                self.bump();
                                self.bump();
                                depth += 1;
                            }
                            (Some('*'), Some('/')) => {
                                self.bump();
                                self.bump();
                                depth -= 1;
                            }
                            (Some(_), _) => {
                                self.bump();
                            }
                            (None, _) => break,
                        }
                    }
                    self.record_comment(own_line, line, start);
                }
                _ => return,
            }
        }
    }

    /// A digit run after a field-access `.` — a tuple element index. Plain
    /// digits only: no fractional part, no duration suffix.
    fn integer_index(&mut self) -> Result<Tok, LexError> {
        let mut text = String::new();
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() {
                text.push(c);
                self.bump();
            } else {
                break;
            }
        }
        let value = text
            .parse::<i64>()
            .map_err(|_| self.err(format!("invalid tuple index `{text}`")))?;
        Ok(Tok::Int(value))
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
        // A duration suffix (`30s`, `2hr`, `1ms`, ...) turns the integer into a
        // Duration literal carried as whole milliseconds. The suffix is the
        // maximal run of letters immediately after the digits; only an exact
        // unit match consumes it (so `1hours` stays `1` then `hours`).
        let mut k = 0;
        while self
            .chars
            .get(self.pos + k)
            .is_some_and(|c| c.is_ascii_alphabetic())
        {
            k += 1;
        }
        if k > 0 {
            let suffix: String = self.chars[self.pos..self.pos + k].iter().collect();
            let unit_ms = match suffix.as_str() {
                "ms" => Some(1_i64),
                "s" => Some(1_000),
                "m" => Some(60_000),
                "h" | "hr" => Some(3_600_000),
                "d" => Some(86_400_000),
                "w" => Some(604_800_000),
                _ => None,
            };
            if let Some(unit_ms) = unit_ms {
                for _ in 0..k {
                    self.bump();
                }
                let ms = value.checked_mul(unit_ms).ok_or_else(|| {
                    self.err(format!("duration literal `{text}{suffix}` overflows"))
                })?;
                return Ok(Tok::Duration(ms));
            }
        }
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
            "fn" => Tok::Fn,
            "gen" => Tok::Gen,
            "yield" => Tok::Yield,
            "async" => Tok::Async,
            "await" => Tok::Await,
            "let" => Tok::Let,
            "var" => Tok::Var,
            "match" => Tok::Match,
            "pub" => Tok::Pub,
            "if" => Tok::If,
            "else" => Tok::Else,
            "true" => Tok::True,
            "false" => Tok::False,
            "type" => Tok::Type,
            "own" => Tok::Own,
            "move" => Tok::Move,
            "import" => Tok::Import,
            "while" => Tok::While,
            "for" => Tok::For,
            "in" => Tok::In,
            "return" => Tok::Return,
            "break" => Tok::Break,
            "continue" => Tok::Continue,
            "trait" => Tok::Trait,
            "impl" => Tok::Impl,
            "where" => Tok::Where,
            "as" => Tok::As,
            "retain" => Tok::Retain,
            "without" => Tok::Without,
            "region" => Tok::Region,
            "comptime" => Tok::Comptime,
            "_" => Tok::Underscore,
            _ => Tok::Ident(text),
        }
    }

    /// Lex a string literal. A plain string is a single `Str` token. A string
    /// with `${ expr }` interpolations expands to the token stream for
    /// `( lit0 + __render(expr0) + lit1 + ... )`, so the parser needs no
    /// special handling and interpolation works in both backends (`to_string` +
    /// concat). Write `\$` for a literal `$`.
    fn string(&mut self, fstring: bool) -> Result<Vec<Tok>, LexError> {
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
                // f-string: `{expr}` interpolates; `{{`/`}}` are literal braces.
                Some('{') if fstring => {
                    if self.peek() == Some('{') {
                        self.bump();
                        text.push('{');
                    } else {
                        self.emit_interpolation(&mut out, &mut text, &mut interpolated)?;
                    }
                }
                Some('}') if fstring => {
                    if self.peek() == Some('}') {
                        self.bump();
                    }
                    text.push('}');
                }
                // plain string: `${expr}` interpolates.
                Some('$') if !fstring && self.peek() == Some('{') => {
                    self.bump(); // consume '{'
                    self.emit_interpolation(&mut out, &mut text, &mut interpolated)?;
                }
                Some(c) => text.push(c),
            }
        }
        if interpolated {
            out.push(Tok::Plus);
            out.push(Tok::Str(text));
            out.push(Tok::RParen);
            Ok(out)
        } else {
            Ok(vec![Tok::Str(text)])
        }
    }

    /// Emit one interpolation segment `<> __render( <expr> )` (the opening brace
    /// already consumed): close off the preceding literal, then read and tokenize
    /// the embedded expression up to its matching `}`.
    fn emit_interpolation(
        &mut self,
        out: &mut Vec<Tok>,
        text: &mut String,
        interpolated: &mut bool,
    ) -> Result<(), LexError> {
        if *interpolated {
            out.push(Tok::Plus);
        } else {
            out.push(Tok::LParen);
            *interpolated = true;
        }
        out.push(Tok::Str(std::mem::take(text)));
        let src = self.interp_source()?;
        let expr_toks = Lexer::new(&src).tokenize()?;
        out.push(Tok::Plus);
        out.push(Tok::Ident("__render".into()));
        out.push(Tok::LParen);
        for t in expr_toks {
            if t.kind == Tok::Eof {
                break;
            }
            out.push(t.kind);
        }
        out.push(Tok::RParen);
        Ok(())
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
                // `\"` — the writer kept the enclosing string's escaping going
                // inside the braces (a natural habit). Honor it: the pair is a
                // quote, opening a nested string that the matching `\"` closes.
                Some('\\') if self.peek() == Some('"') => {
                    self.bump();
                    src.push('"');
                    loop {
                        match self.bump() {
                            None => return Err(self.err("unterminated string in interpolation")),
                            Some('\\') => match self.bump() {
                                Some('"') => {
                                    src.push('"');
                                    break;
                                }
                                Some(c) => {
                                    src.push('\\');
                                    src.push(c);
                                }
                                None => {
                                    return Err(
                                        self.err("unterminated string in interpolation")
                                    );
                                }
                            },
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
            // A bare `{` is not witchy syntax; `}` is only ever an anonymous-struct
            // close (`.{ … }`), so it is allowed and matched by the parser.
            ('{', _) => {
                return Err(self.err(
                    "braces are not part of witchy syntax — use indentation (`:` and an indented block)",
                ))
            }
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
            ('.', Some('{')) => {
                self.bump();
                Tok::DotLBrace
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

/// The own-line comments in `src`, as `(line, text)` in source order — used by
/// the formatter to reproduce comments. Trailing comments (sharing a line with
/// code) are not captured. Returns what was lexed even if a later error occurs.
pub fn own_line_comments(src: &str) -> Vec<(u32, String)> {
    let mut lexer = Lexer::new(src);
    let _ = lexer.tokenize();
    lexer.comments
}

fn vtok(kind: Tok, near: &Token) -> Token {
    Token { kind, line: near.line, col: near.col }
}

/// Off-side-rule layout. Transforms an indentation-delimited token stream into a
/// brace-delimited one: where a `:`-terminated header line is followed by a more
/// deeply indented block, virtual `{`/`}` are inserted (and the header `:` is
/// dropped). The parser, typechecker, interpreter, and codegen are unchanged —
/// they only ever see braces.
///
/// Code that already uses explicit braces passes through untouched, and a line
/// without a trailing `:`/`->` opens no block — so plain multi-line signatures,
/// call arguments, and list literals (which open no block) are unaffected. A
/// `:`/`->` header line that ends *inside* brackets DOES open a block, which is
/// what lets a block-bodied lambda be passed as a call argument
/// (`list.map(xs, fn(c):` then an indented `match`/body). Such an inner block is
/// closed either by a dedent or by the bracket that encloses it closing.
pub fn apply_layout(tokens: Vec<Token>) -> Vec<Token> {
    struct LayoutLine {
        indent: u32,
        /// Bracket nesting depth at the line's first token.
        bdepth_start: i32,
        toks: Vec<Token>,
    }

    // Phase 1: group tokens into source lines, recording the bracket depth each
    // line starts at. We DON'T suppress line breaks inside brackets — a header
    // line that ends with `:`/`->` opens a block wherever it appears, while a
    // continuation line (no trailing header) opens none, so ordinary multi-line
    // calls and signatures still flow through as a single token sequence.
    let mut lines: Vec<LayoutLine> = Vec::new();
    let mut eof: Option<Token> = None;
    let mut depth: i32 = 0;
    let mut prev_line: u32 = 0;
    for t in tokens {
        if t.kind == Tok::Eof {
            eof = Some(t);
            break;
        }
        let starts_line = lines.is_empty() || t.line != prev_line;
        prev_line = t.line;
        let kind = t.kind.clone();
        if starts_line {
            lines.push(LayoutLine { indent: t.col, bdepth_start: depth, toks: vec![t] });
        } else {
            lines.last_mut().unwrap().toks.push(t);
        }
        match kind {
            Tok::LParen | Tok::LBracket | Tok::LBrace | Tok::DotLBrace => depth += 1,
            Tok::RParen | Tok::RBracket | Tok::RBrace => depth -= 1,
            _ => {}
        }
    }

    // Phase 2: open a virtual block after each `:`/`->` header; close blocks on
    // dedent and when their enclosing bracket closes. `block_bd[i]` is the
    // bracket depth `stack[i]`'s block opened at, so a block opened inside a call
    // is torn down when that call's `)` is reached, and an indent-dedent never
    // closes a block from a shallower bracket level than the current line (which
    // would wrongly close an outer block on a dedented continuation line).
    let mut out: Vec<Token> = Vec::new();
    let mut stack: Vec<u32> = Vec::new(); // header indents of open virtual blocks
    let mut block_bd: Vec<i32> = Vec::new(); // bracket depth each block opened at
    let mut pending: Option<u32> = None; // header indent awaiting its block body
    let mut bdepth: i32 = 0;
    for line in &lines {
        let here = &line.toks[0];
        if let Some(header_indent) = pending.take() {
            if line.indent > header_indent {
                out.push(vtok(Tok::LBrace, here));
                stack.push(header_indent);
                block_bd.push(bdepth);
            } else {
                // A `:` header with no indented body: an empty block.
                out.push(vtok(Tok::LBrace, here));
                out.push(vtok(Tok::RBrace, here));
            }
        }
        // Indent-based dedent. Close the top block while this line is no more
        // indented than its header AND the block was opened at this line's
        // bracket level or deeper (so a dedented continuation line inside a call
        // can't close a block that lives outside the call).
        while stack.last().is_some_and(|top| line.indent <= *top)
            && block_bd.last().is_some_and(|bd| *bd >= line.bdepth_start)
        {
            let near = out.last().cloned().unwrap_or_else(|| here.clone());
            out.push(vtok(Tok::RBrace, &near));
            stack.pop();
            block_bd.pop();
        }
        // Emit the line's tokens, tracking bracket depth. Before a closing
        // bracket drops the depth, tear down any block opened deeper than the new
        // depth — the block-bodied-lambda case where the call paren closes on the
        // same line as (or before) the block would otherwise dedent.
        let n = line.toks.len();
        let ends_with_arrow = matches!(line.toks.last().map(|t| &t.kind), Some(Tok::RArrow));
        for (i, t) in line.toks.iter().enumerate() {
            if i == n - 1 && t.kind == Tok::Colon {
                // Drop a trailing `:` header; its block opens on the next line.
                pending = Some(line.indent);
                break;
            }
            match t.kind {
                Tok::RParen | Tok::RBracket | Tok::RBrace => {
                    let new_bd = bdepth - 1;
                    while block_bd.last().is_some_and(|bd| *bd > new_bd) {
                        let near = out.last().cloned().unwrap_or_else(|| t.clone());
                        out.push(vtok(Tok::RBrace, &near));
                        stack.pop();
                        block_bd.pop();
                    }
                    bdepth = new_bd;
                    out.push(t.clone());
                }
                Tok::LParen | Tok::LBracket | Tok::LBrace | Tok::DotLBrace => {
                    bdepth += 1;
                    out.push(t.clone());
                }
                _ => out.push(t.clone()),
            }
        }
        if ends_with_arrow {
            // A match-arm `->` is kept (part of the grammar); its block opens next.
            pending = Some(line.indent);
        }
    }

    if let Some(eof_tok) = eof {
        if pending.take().is_some() {
            let near = out.last().cloned().unwrap_or_else(|| eof_tok.clone());
            out.push(vtok(Tok::LBrace, &near));
            out.push(vtok(Tok::RBrace, &near));
        }
        while stack.pop().is_some() {
            let near = out.last().cloned().unwrap_or_else(|| eof_tok.clone());
            out.push(vtok(Tok::RBrace, &near));
        }
        out.push(eof_tok);
    }
    out
}

#[cfg(test)]
#[path = "lexer_tests.rs"]
mod tests;
