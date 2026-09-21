//! The line-wrap vocabulary shared by `gpui`'s line wrapper and its callers.
//!
//! `LineWrapper` itself stays in `gpui` because it borrows the text system, which
//! `gpui` still owns; the plain-data fragments and boundaries it operates on live
//! here so a caller can describe what to wrap without depending on `gpui`.

use gpui_types::Pixels;

/// A fragment of a line that can be wrapped.
pub enum LineFragment<'a> {
    /// A text fragment consisting of characters.
    Text {
        /// The text content of the fragment.
        text: &'a str,
    },
    /// A non-text element with a fixed width.
    Element {
        /// The width of the element in pixels.
        width: Pixels,
        /// The UTF-8 encoded length of the element.
        len_utf8: usize,
    },
}

impl<'a> LineFragment<'a> {
    /// Creates a new text fragment from the given text.
    pub fn text(text: &'a str) -> Self {
        LineFragment::Text { text }
    }

    /// Creates a new non-text element with the given width and UTF-8 encoded length.
    pub fn element(width: Pixels, len_utf8: usize) -> Self {
        LineFragment::Element { width, len_utf8 }
    }

    /// The boundary candidates this fragment contributes: one per character for a text
    /// fragment, or a single element candidate.
    pub fn wrap_boundary_candidates(&self) -> impl Iterator<Item = WrapBoundaryCandidate> {
        let text = match self {
            LineFragment::Text { text } => text,
            LineFragment::Element { .. } => "\0",
        };
        text.chars().map(move |character| {
            if let LineFragment::Element { width, len_utf8 } = self {
                WrapBoundaryCandidate::Element {
                    width: *width,
                    len_utf8: *len_utf8,
                }
            } else {
                WrapBoundaryCandidate::Char { character }
            }
        })
    }
}

/// A candidate position at which a line may be wrapped.
pub enum WrapBoundaryCandidate {
    /// A boundary after a character.
    Char {
        /// The character the boundary follows.
        character: char,
    },
    /// A boundary after a non-text element.
    Element {
        /// The width of the element in pixels.
        width: Pixels,
        /// The UTF-8 encoded length of the element.
        len_utf8: usize,
    },
}

impl WrapBoundaryCandidate {
    /// The UTF-8 encoded length of this candidate.
    pub fn len_utf8(&self) -> usize {
        match self {
            WrapBoundaryCandidate::Char { character } => character.len_utf8(),
            WrapBoundaryCandidate::Element { len_utf8: len, .. } => *len,
        }
    }
}

/// A boundary between two lines of text.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Boundary {
    /// The index of the last character in a line
    pub ix: usize,
    /// The indent of the next line.
    pub next_indent: u32,
}

impl Boundary {
    /// Creates a boundary at `ix` with `next_indent` applied to the following line.
    pub fn new(ix: usize, next_indent: u32) -> Self {
        Self { ix, next_indent }
    }
}
