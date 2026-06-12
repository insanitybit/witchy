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
    Actor,
    Fn,
    Gen,
    Yield,
    Let,
    Var,
    On,
    Match,
    Pub,
    If,
    Else,
    True,
    False,
    Spawn,
    Type,
    Inout,
    Sink,
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
            Duration(ms) => write!(f, "{ms}ms"),
            Str(s) => write!(f, "{s:?}"),
            Ident(s) => write!(f, "{s}"),
            Actor => write!(f, "actor"),
            Fn => write!(f, "fn"),
            Gen => write!(f, "gen"),
            Yield => write!(f, "yield"),
            Let => write!(f, "let"),
            Var => write!(f, "var"),
            On => write!(f, "on"),
            Match => write!(f, "match"),
            Pub => write!(f, "pub"),
            If => write!(f, "if"),
            Else => write!(f, "else"),
            True => write!(f, "true"),
            False => write!(f, "false"),
            Spawn => write!(f, "spawn"),
            Type => write!(f, "type"),
            Inout => write!(f, "inout"),
            Sink => write!(f, "sink"),
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
            "actor" => Tok::Actor,
            "fn" => Tok::Fn,
            "gen" => Tok::Gen,
            "yield" => Tok::Yield,
            "let" => Tok::Let,
            "var" => Tok::Var,
            "on" => Tok::On,
            "match" => Tok::Match,
            "pub" => Tok::Pub,
            "if" => Tok::If,
            "else" => Tok::Else,
            "true" => Tok::True,
            "false" => Tok::False,
            "spawn" => Tok::Spawn,
            "type" => Tok::Type,
            "inout" => Tok::Inout,
            "sink" => Tok::Sink,
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
    /// `( lit0 <> to_string(expr0) <> lit1 <> ... )`, so the parser needs no
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
            out.push(Tok::Concat);
            out.push(Tok::Str(text));
            out.push(Tok::RParen);
            Ok(out)
        } else {
            Ok(vec![Tok::Str(text)])
        }
    }

    /// Emit one interpolation segment `<> to_string( <expr> )` (the opening brace
    /// already consumed): close off the preceding literal, then read and tokenize
    /// the embedded expression up to its matching `}`.
    fn emit_interpolation(
        &mut self,
        out: &mut Vec<Tok>,
        text: &mut String,
        interpolated: &mut bool,
    ) -> Result<(), LexError> {
        if *interpolated {
            out.push(Tok::Concat);
        } else {
            out.push(Tok::LParen);
            *interpolated = true;
        }
        out.push(Tok::Str(std::mem::take(text)));
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
            ('{', _) | ('}', _) => {
                return Err(self.err(
                    "braces are not part of witchy syntax — use indentation (`:` and an indented block)",
                ))
            }
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
/// Code that already uses explicit braces passes through untouched: inside `()`,
/// `[]`, or `{}` layout is suppressed, and a line without a trailing `:` opens no
/// block. So the two styles coexist, which is what lets the migration be gradual.
pub fn apply_layout(tokens: Vec<Token>) -> Vec<Token> {
    struct LayoutLine {
        indent: u32,
        toks: Vec<Token>,
    }

    // Phase 1: group tokens into layout lines. A new line begins at a token that
    // starts a fresh source line while at bracket depth 0 (brackets and explicit
    // braces suppress layout, keeping multi-line signatures and brace blocks
    // whole).
    let mut lines: Vec<LayoutLine> = Vec::new();
    let mut eof: Option<Token> = None;
    let mut depth: i32 = 0;
    let mut prev_line: u32 = 0;
    for t in tokens {
        if t.kind == Tok::Eof {
            eof = Some(t);
            break;
        }
        let starts_line = lines.is_empty() || (depth == 0 && t.line != prev_line);
        prev_line = t.line;
        match t.kind {
            Tok::LParen | Tok::LBracket | Tok::LBrace => depth += 1,
            Tok::RParen | Tok::RBracket | Tok::RBrace => depth -= 1,
            _ => {}
        }
        if starts_line {
            lines.push(LayoutLine { indent: t.col, toks: vec![t] });
        } else {
            lines.last_mut().unwrap().toks.push(t);
        }
    }

    // Phase 2: open a virtual block after each `:`-terminated header and close
    // blocks on dedent.
    let mut out: Vec<Token> = Vec::new();
    let mut stack: Vec<u32> = Vec::new(); // header indents of open virtual blocks
    let mut pending: Option<u32> = None; // header indent awaiting its block body
    for line in &lines {
        let here = &line.toks[0];
        if let Some(header_indent) = pending.take() {
            if line.indent > header_indent {
                out.push(vtok(Tok::LBrace, here));
                stack.push(header_indent);
            } else {
                // A `:` header with no indented body: an empty block.
                out.push(vtok(Tok::LBrace, here));
                out.push(vtok(Tok::RBrace, here));
            }
        }
        // A closing `}` is placed on the PREVIOUS token's line, never the
        // dedent line's, so the parser doesn't read a following `(...)` as
        // applying the block's value (e.g. `} \n (a, b)` must stay two
        // statements, not `}(a, b)`).
        while stack.last().is_some_and(|top| line.indent <= *top) {
            let near = out.last().cloned().unwrap_or_else(|| here.clone());
            out.push(vtok(Tok::RBrace, &near));
            stack.pop();
        }
        // A trailing `:` or `->` opens a virtual block for the indented body. The
        // `:` is a pure block header and is dropped; a match-arm `->` is part of
        // the grammar, so it is kept and the block opens right after it (giving a
        // multi-statement match-arm body without braces).
        match line.toks.last().map(|t| &t.kind) {
            Some(Tok::Colon) => {
                out.extend(line.toks[..line.toks.len() - 1].iter().cloned());
                pending = Some(line.indent);
            }
            Some(Tok::RArrow) => {
                out.extend(line.toks.iter().cloned());
                pending = Some(line.indent);
            }
            _ => out.extend(line.toks.iter().cloned()),
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
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<Tok> {
        tokenize(src).unwrap().into_iter().map(|t| t.kind).collect()
    }

    #[test]
    fn captures_own_line_comments_not_trailing() {
        let src = "// header\nfn f() -> Int:\n    // inner\n    5 // trailing\n/* block */\n";
        let cs = own_line_comments(src);
        assert_eq!(
            cs,
            vec![
                (1, "// header".to_string()),
                (3, "// inner".to_string()),
                (5, "/* block */".to_string()),
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
        let src = "fn greet(name: String) -> String: \"hi, \" <> name";
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
                Tok::Concat,
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
}
