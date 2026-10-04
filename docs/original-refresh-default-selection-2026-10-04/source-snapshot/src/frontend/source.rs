//! Source files, [`FileId`], [`Span`], and the expansion-origin seam.
//!
//! Byte offsets are the canonical position representation: every token,
//! syntax node, and diagnostic carries a [`Span`] of byte offsets into one
//! [`FileId`]. Hosts derive line and column with [`line_column`] when they
//! need to display a position; nothing stores two coordinate systems.
//!
//! `Span::expansion` is always `None` in version 1. It exists so that the
//! future preprocessing pass can record macro invocation and spelling spans
//! without changing every consumer of a span.

use serde::{Deserialize, Serialize};

/// Index of one source file inside a compile call's source set, in the
/// order the caller passed them to `compile_systemverilog`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct FileId(pub u32);

/// Index of one macro expansion in the future expansion table. Never
/// produced in version 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ExpansionId(pub u32);

/// A byte range `start..end` inside one source file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Span {
    pub file: FileId,
    pub start: u32,
    pub end: u32,
    pub expansion: Option<ExpansionId>,
}

impl Span {
    /// A span with no expansion origin -- the only kind version 1 makes.
    pub fn new(file: FileId, start: u32, end: u32) -> Span {
        Span {
            file,
            start,
            end,
            expansion: None,
        }
    }

    /// The smallest span covering both `self` and `other`. Both must be in
    /// the same file; a parser only ever joins spans of one file.
    pub fn join(self, other: Span) -> Span {
        debug_assert_eq!(self.file, other.file);
        Span {
            file: self.file,
            start: self.start.min(other.start),
            end: self.end.max(other.end),
            expansion: None,
        }
    }

    /// Whether `inner` lies entirely inside `self` (same file).
    pub fn contains(self, inner: Span) -> bool {
        self.file == inner.file && self.start <= inner.start && inner.end <= self.end
    }
}

/// One source file handed to the compiler. Borrowed, so a host can pass its
/// editor buffer straight through without copying.
#[derive(Debug, Clone, Copy)]
pub struct SourceInput<'a> {
    /// A display name for diagnostics -- a path, or anything the host uses
    /// to identify the buffer. The compiler never opens it.
    pub name: &'a str,
    pub text: &'a str,
}

/// What the debug database remembers about each source file: enough to
/// map a [`FileId`] back to the host's name and to bounds-check offsets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceFileInfo {
    pub name: String,
    /// Length of the file's text in bytes.
    pub len: u32,
}

/// Derive a 1-based `(line, column)` pair from a byte offset into `text`.
/// Column counts characters, not bytes, so a host can put a caret under the
/// right glyph. An offset past the end reports the position after the last
/// character.
pub fn line_column(text: &str, offset: u32) -> (u32, u32) {
    let mut offset = (offset as usize).min(text.len());
    // Public callers may hand us an arbitrary byte offset. Compiler-created
    // spans are always on character boundaries, but diagnostics must not
    // panic if a host provides an offset in the middle of UTF-8.
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    let before = &text[..offset];
    let line = before.bytes().filter(|&b| b == b'\n').count() as u32 + 1;
    let line_start = before.rfind('\n').map_or(0, |i| i + 1);
    let column = before[line_start..].chars().count() as u32 + 1;
    (line, column)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_and_column_are_one_based_and_count_characters() {
        let text = "ab\ncdé f\n";
        assert_eq!(line_column(text, 0), (1, 1));
        assert_eq!(line_column(text, 3), (2, 1));
        // `é` is two bytes but one character: the space after it is byte 7
        // and character 4, and `f` behind it is byte 8 and character 5.
        assert_eq!(line_column(text, 7), (2, 4));
        assert_eq!(line_column(text, 8), (2, 5));
        assert_eq!(line_column(text, 6), (2, 3));
        assert_eq!(line_column(text, 999), (3, 1));
    }

    #[test]
    fn join_and_contains() {
        let a = Span::new(FileId(0), 4, 8);
        let b = Span::new(FileId(0), 6, 12);
        assert_eq!(a.join(b), Span::new(FileId(0), 4, 12));
        assert!(a.join(b).contains(a));
        assert!(!a.contains(b));
        assert!(!Span::new(FileId(1), 0, 100).contains(a));
    }
}
