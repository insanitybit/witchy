//! Source identities survive ordered library concatenation without changing AST offsets.
use std::ops::Range;

#[derive(Clone, Debug)]
pub struct SourceFile {
    pub path: String,
    pub text: String,
    pub span: Range<usize>,
}

#[derive(Clone, Debug, Default)]
pub struct SourceFiles {
    text: String,
    files: Vec<SourceFile>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceLocation {
    pub path: String,
    pub line: usize,
    pub column: usize,
    pub line_text: String,
}

impl SourceFiles {
    pub fn push(&mut self, path: impl Into<String>, text: String) {
        let start = self.text.len();
        self.text.push_str(&text);
        let end = self.text.len();
        self.text.push('\n');
        self.files.push(SourceFile { path: path.into(), text, span: start..end });
    }

    pub fn text(&self) -> &str { &self.text }

    /// Exact framed source identity for checked-artifact reuse. No paths or
    /// source bytes can alias through ambiguous separators or concatenation.
    pub fn identity_bytes(&self) -> Vec<u8> {
        fn frame(out: &mut Vec<u8>, bytes: &[u8]) {
            out.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
            out.extend_from_slice(bytes);
        }
        let mut out = b"witchy-ordered-source-files-v1".to_vec();
        out.extend_from_slice(&(self.files.len() as u64).to_le_bytes());
        for file in &self.files {
            frame(&mut out, file.path.as_bytes());
            frame(&mut out, file.text.as_bytes());
            out.extend_from_slice(&(file.span.start as u64).to_le_bytes());
            out.extend_from_slice(&(file.span.end as u64).to_le_bytes());
        }
        out
    }

    pub fn location(&self, offset: usize) -> Option<SourceLocation> {
        let file = self.files.iter().find(|file| file.span.contains(&offset))
            .or_else(|| self.files.iter().rev().find(|file| file.span.end == offset))?;
        let local = offset.checked_sub(file.span.start)?.min(file.text.len());
        if !file.text.is_char_boundary(local) { return None; }
        let prefix = &file.text[..local];
        let line = prefix.bytes().filter(|byte| *byte == b'\n').count() + 1;
        let column = prefix.rsplit('\n').next().unwrap_or("").chars().count() + 1;
        let line_text = file.text.lines().nth(line - 1).unwrap_or("").to_owned();
        Some(SourceLocation { path: file.path.clone(), line, column, line_text })
    }

    pub fn render(&self, diagnostic: &crate::diagnostic::Diagnostic) -> String {
        if diagnostic.span == (0..0) {
            return format!("{}: {} (source location unavailable)", diagnostic.code, diagnostic.message);
        }
        match self.location(diagnostic.span.start) {
            Some(at) => format!("{}:{}:{}: {}: {}\n  {}", at.path, at.line, at.column,
                diagnostic.code, diagnostic.message, at.line_text),
            None => format!("{} {:?}: {}", diagnostic.code, diagnostic.span, diagnostic.message),
        }
    }
}
