//! Hand-rolled SystemVerilog lexer for the subset REDA compiles.
//!
//! Every token carries a [`Span`]. Whitespace and both comment forms are
//! skipped, so they never influence token spans other than by moving them
//! -- which is exactly why semantic IDs derived from token *order* are
//! comment-insensitive while byte spans are not. The lexer never panics on
//! arbitrary UTF-8: an unrecognised character is a diagnostic, not a crash.

use super::source::{FileId, Span};
use super::{Diagnostic, Severity};

/// Keywords the parser distinguishes from identifiers. Everything the
/// version 1 grammar mentions is here, including the constructs the
/// elaborator rejects, so a rejection can name the keyword it saw instead
/// of failing as "unexpected identifier".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Keyword {
    Module,
    Endmodule,
    Input,
    Output,
    Inout,
    Wire,
    Logic,
    Reg,
    Assign,
    AlwaysComb,
    AlwaysFf,
    Always,
    Posedge,
    Negedge,
    Or,
    Begin,
    End,
    If,
    Else,
    Case,
    Casez,
    Casex,
    Endcase,
    Default,
    Parameter,
    Localparam,
}

impl Keyword {
    fn from_word(word: &str) -> Option<Keyword> {
        Some(match word {
            "module" => Keyword::Module,
            "endmodule" => Keyword::Endmodule,
            "input" => Keyword::Input,
            "output" => Keyword::Output,
            "inout" => Keyword::Inout,
            "wire" => Keyword::Wire,
            "logic" => Keyword::Logic,
            "reg" => Keyword::Reg,
            "assign" => Keyword::Assign,
            "always_comb" => Keyword::AlwaysComb,
            "always_ff" => Keyword::AlwaysFf,
            "always" => Keyword::Always,
            "posedge" => Keyword::Posedge,
            "negedge" => Keyword::Negedge,
            "or" => Keyword::Or,
            "begin" => Keyword::Begin,
            "end" => Keyword::End,
            "if" => Keyword::If,
            "else" => Keyword::Else,
            "case" => Keyword::Case,
            "casez" => Keyword::Casez,
            "casex" => Keyword::Casex,
            "endcase" => Keyword::Endcase,
            "default" => Keyword::Default,
            "parameter" => Keyword::Parameter,
            "localparam" => Keyword::Localparam,
            _ => return None,
        })
    }

    /// The keyword's spelling, for diagnostics.
    pub fn as_str(self) -> &'static str {
        match self {
            Keyword::Module => "module",
            Keyword::Endmodule => "endmodule",
            Keyword::Input => "input",
            Keyword::Output => "output",
            Keyword::Inout => "inout",
            Keyword::Wire => "wire",
            Keyword::Logic => "logic",
            Keyword::Reg => "reg",
            Keyword::Assign => "assign",
            Keyword::AlwaysComb => "always_comb",
            Keyword::AlwaysFf => "always_ff",
            Keyword::Always => "always",
            Keyword::Posedge => "posedge",
            Keyword::Negedge => "negedge",
            Keyword::Or => "or",
            Keyword::Begin => "begin",
            Keyword::End => "end",
            Keyword::If => "if",
            Keyword::Else => "else",
            Keyword::Case => "case",
            Keyword::Casez => "casez",
            Keyword::Casex => "casex",
            Keyword::Endcase => "endcase",
            Keyword::Default => "default",
            Keyword::Parameter => "parameter",
            Keyword::Localparam => "localparam",
        }
    }
}

/// Punctuation and operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Punct {
    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace,
    RBrace,
    Semicolon,
    Comma,
    Colon,
    Dot,
    At,
    Hash,
    Question,
    /// `=`
    Assign,
    /// `<=`
    LessEqual,
    /// `~`
    Tilde,
    /// `!`
    Bang,
    /// `&`
    Amp,
    /// `|`
    Pipe,
    /// `^`
    Caret,
    /// `&&`
    AndAnd,
    /// `||`
    OrOr,
    /// `==`
    EqualEqual,
    /// `!=`
    BangEqual,
}

impl Punct {
    /// The punctuation's spelling, for diagnostics.
    pub fn as_str(self) -> &'static str {
        match self {
            Punct::LParen => "(",
            Punct::RParen => ")",
            Punct::LBracket => "[",
            Punct::RBracket => "]",
            Punct::LBrace => "{",
            Punct::RBrace => "}",
            Punct::Semicolon => ";",
            Punct::Comma => ",",
            Punct::Colon => ":",
            Punct::Dot => ".",
            Punct::At => "@",
            Punct::Hash => "#",
            Punct::Question => "?",
            Punct::Assign => "=",
            Punct::LessEqual => "<=",
            Punct::Tilde => "~",
            Punct::Bang => "!",
            Punct::Amp => "&",
            Punct::Pipe => "|",
            Punct::Caret => "^",
            Punct::AndAnd => "&&",
            Punct::OrOr => "||",
            Punct::EqualEqual => "==",
            Punct::BangEqual => "!=",
        }
    }
}

/// A number literal as spelled. Interpretation (width, base, value) is the
/// parser's job so a malformed literal gets a parser diagnostic with the
/// literal's full span.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Number {
    /// A bare decimal integer such as `3` in `[3:0]`.
    Unsized(String),
    /// `width'<base><digits>`, e.g. `4'b0101` or `7'd12`. `digits` keeps
    /// `_` separators and any character the base does not accept, so the
    /// parser can report exactly what was wrong.
    Sized {
        width: String,
        base: char,
        digits: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenKind {
    Ident(String),
    Keyword(Keyword),
    Punct(Punct),
    Number(Number),
    /// End of file. Always the last token, so the parser can point a
    /// "expected X" diagnostic at the end of input.
    Eof,
}

impl TokenKind {
    /// A short description for "expected X, found Y" diagnostics.
    pub fn describe(&self) -> String {
        match self {
            TokenKind::Ident(name) => format!("identifier `{name}`"),
            TokenKind::Keyword(keyword) => format!("keyword `{}`", keyword.as_str()),
            TokenKind::Punct(punct) => format!("`{}`", punct.as_str()),
            TokenKind::Number(Number::Unsized(text)) => format!("number `{text}`"),
            TokenKind::Number(Number::Sized {
                width,
                base,
                digits,
            }) => format!("number `{width}'{base}{digits}`"),
            TokenKind::Eof => "end of file".to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

/// Tokenise one file. Stops at the first lexical error and returns it as
/// the single diagnostic, matching the parser's no-recovery policy.
pub fn tokenize(file: FileId, text: &str) -> Result<Vec<Token>, Diagnostic> {
    let mut tokens = Vec::new();
    let bytes = text.as_bytes();
    let mut pos = 0usize;

    let error = |start: usize, end: usize, message: String| Diagnostic {
        severity: Severity::Error,
        message,
        span: Span::new(file, start as u32, end as u32),
    };

    while pos < bytes.len() {
        let c = bytes[pos];

        // Whitespace.
        if c.is_ascii_whitespace() {
            pos += 1;
            continue;
        }

        // Comments.
        if c == b'/' && pos + 1 < bytes.len() {
            match bytes[pos + 1] {
                b'/' => {
                    while pos < bytes.len() && bytes[pos] != b'\n' {
                        pos += 1;
                    }
                    continue;
                }
                b'*' => {
                    let start = pos;
                    pos += 2;
                    loop {
                        if pos + 1 >= bytes.len() {
                            return Err(error(
                                start,
                                bytes.len(),
                                "unterminated block comment".to_string(),
                            ));
                        }
                        if bytes[pos] == b'*' && bytes[pos + 1] == b'/' {
                            pos += 2;
                            break;
                        }
                        pos += 1;
                    }
                    continue;
                }
                _ => {}
            }
        }

        let start = pos;

        // Identifiers and keywords.
        if c.is_ascii_alphabetic() || c == b'_' {
            while pos < bytes.len()
                && (bytes[pos].is_ascii_alphanumeric() || bytes[pos] == b'_' || bytes[pos] == b'$')
            {
                pos += 1;
            }
            let word = &text[start..pos];
            let kind = match Keyword::from_word(word) {
                Some(keyword) => TokenKind::Keyword(keyword),
                None => TokenKind::Ident(word.to_string()),
            };
            tokens.push(Token {
                kind,
                span: Span::new(file, start as u32, pos as u32),
            });
            continue;
        }

        // Numbers: `123`, or `N'bxxxx` / `N'dNNN` (also `'h`/`'o` so a
        // wrong base is reported as such, not as stray characters).
        if c.is_ascii_digit() {
            while pos < bytes.len() && (bytes[pos].is_ascii_digit() || bytes[pos] == b'_') {
                pos += 1;
            }
            let width = &text[start..pos];
            if pos < bytes.len() && bytes[pos] == b'\'' {
                let base_pos = pos + 1;
                let base = match bytes.get(base_pos) {
                    Some(&b) if b.is_ascii_alphabetic() => b.to_ascii_lowercase() as char,
                    _ => {
                        return Err(error(
                            start,
                            base_pos.min(bytes.len()),
                            "sized literal needs a base letter after `'` (`'b` or `'d`)"
                                .to_string(),
                        ));
                    }
                };
                pos = base_pos + 1;
                let digits_start = pos;
                while pos < bytes.len()
                    && (bytes[pos].is_ascii_alphanumeric()
                        || bytes[pos] == b'_'
                        || bytes[pos] == b'?')
                {
                    pos += 1;
                }
                if pos == digits_start {
                    return Err(error(
                        start,
                        pos,
                        "sized literal has no digits after its base".to_string(),
                    ));
                }
                tokens.push(Token {
                    kind: TokenKind::Number(Number::Sized {
                        width: width.to_string(),
                        base,
                        digits: text[digits_start..pos].to_string(),
                    }),
                    span: Span::new(file, start as u32, pos as u32),
                });
            } else {
                tokens.push(Token {
                    kind: TokenKind::Number(Number::Unsized(width.to_string())),
                    span: Span::new(file, start as u32, pos as u32),
                });
            }
            continue;
        }

        // Two-character operators first, then single characters.
        let two = if pos + 1 < bytes.len() {
            Some((c, bytes[pos + 1]))
        } else {
            None
        };
        let (punct, len) = match two {
            Some((b'&', b'&')) => (Punct::AndAnd, 2),
            Some((b'|', b'|')) => (Punct::OrOr, 2),
            Some((b'=', b'=')) => (Punct::EqualEqual, 2),
            Some((b'!', b'=')) => (Punct::BangEqual, 2),
            Some((b'<', b'=')) => (Punct::LessEqual, 2),
            _ => match c {
                b'(' => (Punct::LParen, 1),
                b')' => (Punct::RParen, 1),
                b'[' => (Punct::LBracket, 1),
                b']' => (Punct::RBracket, 1),
                b'{' => (Punct::LBrace, 1),
                b'}' => (Punct::RBrace, 1),
                b';' => (Punct::Semicolon, 1),
                b',' => (Punct::Comma, 1),
                b':' => (Punct::Colon, 1),
                b'.' => (Punct::Dot, 1),
                b'@' => (Punct::At, 1),
                b'#' => (Punct::Hash, 1),
                b'?' => (Punct::Question, 1),
                b'=' => (Punct::Assign, 1),
                b'~' => (Punct::Tilde, 1),
                b'!' => (Punct::Bang, 1),
                b'&' => (Punct::Amp, 1),
                b'|' => (Punct::Pipe, 1),
                b'^' => (Punct::Caret, 1),
                _ => {
                    // Report the whole UTF-8 character, never a byte
                    // inside one, so the span stays on a char boundary.
                    let ch = text[start..].chars().next().unwrap_or('\u{FFFD}');
                    let end = start + ch.len_utf8();
                    return Err(error(
                        start,
                        end,
                        format!("unexpected character `{}`", ch.escape_default()),
                    ));
                }
            },
        };
        pos += len;
        tokens.push(Token {
            kind: TokenKind::Punct(punct),
            span: Span::new(file, start as u32, pos as u32),
        });
    }

    tokens.push(Token {
        kind: TokenKind::Eof,
        span: Span::new(file, bytes.len() as u32, bytes.len() as u32),
    });
    Ok(tokens)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(text: &str) -> Vec<TokenKind> {
        tokenize(FileId(0), text)
            .expect("lexes")
            .into_iter()
            .map(|t| t.kind)
            .collect()
    }

    #[test]
    fn keywords_operators_and_literals() {
        let got = kinds("module m(input logic a); assign y = a && 4'b01_10; endmodule");
        assert_eq!(got[0], TokenKind::Keyword(Keyword::Module));
        assert_eq!(got[1], TokenKind::Ident("m".into()));
        assert!(got.contains(&TokenKind::Punct(Punct::AndAnd)));
        assert!(got.contains(&TokenKind::Number(Number::Sized {
            width: "4".into(),
            base: 'b',
            digits: "01_10".into(),
        })));
        assert_eq!(got.last(), Some(&TokenKind::Eof));
    }

    #[test]
    fn comments_move_spans_but_leave_tokens_alone() {
        let plain = tokenize(FileId(0), "assign y = a;").unwrap();
        let commented = tokenize(FileId(0), "/* c */ assign y = /* d */ a; // e").unwrap();
        let plain_kinds: Vec<_> = plain.iter().map(|t| &t.kind).collect();
        let commented_kinds: Vec<_> = commented.iter().map(|t| &t.kind).collect();
        assert_eq!(plain_kinds, commented_kinds);
        assert_ne!(plain[0].span, commented[0].span);
    }

    #[test]
    fn errors_have_spans_and_never_panic() {
        let err = tokenize(FileId(0), "assign /* never closed").unwrap_err();
        assert_eq!(err.message, "unterminated block comment");
        assert_eq!((err.span.start, err.span.end), (7, 22));

        let err = tokenize(FileId(0), "a § b").unwrap_err();
        assert_eq!((err.span.start, err.span.end), (2, 4));

        // Arbitrary bytes-as-UTF-8: only Ok or Err, never a panic.
        for text in ["", "'", "4'", "4'b", "\u{1F600}", "//", "/*", "/", "9'zz"] {
            let _ = tokenize(FileId(0), text);
        }
    }
}
