//! Rendered content.
//!
//! Markdown is parsed here, never in the view — `spec/30-ui.md` §1. The parser
//! itself lands with the first surface that renders prose (M2); this module
//! fixes the shape it must produce.

use serde::{Deserialize, Serialize};

/// A styled run of text. Syntax highlighting resolves to these in the model,
/// so the view never sees a language name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StyledSpan {
    pub text: String,
    pub style: SpanStyle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpanStyle {
    #[default]
    Plain,
    Emphasis,
    Strong,
    Code,
    Link,
    /// A resolved `@mention` — GitHub-flavoured, absent from CommonMark.
    Mention,
    /// A resolved `#123` reference — likewise.
    Reference,
    /// A syntax-highlighting class, resolved from a theme at parse time.
    Token(SyntaxToken),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyntaxToken {
    Keyword,
    Type,
    Function,
    String,
    Number,
    Comment,
    Punctuation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Block {
    Paragraph(Vec<StyledSpan>),
    Heading {
        level: u8,
        spans: Vec<StyledSpan>,
    },
    Code {
        language: Option<String>,
        lines: Vec<Vec<StyledSpan>>,
    },
    Quote(Vec<Block>),
    List {
        ordered: bool,
        items: Vec<Vec<Block>>,
    },
    /// GitHub-flavoured task list item. Rendered as a checkbox, not literal
    /// `- [ ]` text.
    Task {
        checked: bool,
        spans: Vec<StyledSpan>,
    },
    Table {
        header: Vec<Vec<StyledSpan>>,
        rows: Vec<Vec<Vec<StyledSpan>>>,
    },
    Rule,
    /// An image reference. Terminals render these only where a graphics
    /// protocol is available; otherwise the alt text stands in.
    Image {
        alt: String,
        url: String,
    },
}

/// Markdown, retaining its source so it can be edited and re-submitted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Markdown {
    pub source: String,
    /// Empty until parsed.
    pub blocks: Vec<Block>,
}

impl Markdown {
    /// Hold the source without parsing it.
    pub fn from_source(source: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            blocks: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.source.trim().is_empty()
    }

    /// A one-line summary for list contexts, with markup flattened away.
    ///
    /// Deliberately crude: enough for a preview line, never a substitute for
    /// rendering.
    pub fn summary(&self, max_chars: usize) -> String {
        let flat = self
            .source
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty() && !l.starts_with(['#', '>', '-', '*', '`']))
            .unwrap_or("")
            .replace(['`', '*', '_'], "");
        if flat.chars().count() <= max_chars {
            return flat;
        }
        let cut: String = flat.chars().take(max_chars.saturating_sub(1)).collect();
        format!("{}…", cut.trim_end())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_skips_headings_and_markup() {
        let md = Markdown::from_source("# Title\n\nThe **actual** body text.\n");
        assert_eq!(md.summary(80), "The actual body text.");
    }

    #[test]
    fn summary_truncates_on_character_boundaries() {
        let md = Markdown::from_source("ünïcödé every single one of them counts");
        let s = md.summary(10);
        assert_eq!(s.chars().count(), 10);
        assert!(s.ends_with('…'));
    }

    #[test]
    fn empty_and_whitespace_only_are_empty() {
        assert!(Markdown::from_source("").is_empty());
        assert!(Markdown::from_source("   \n\n ").is_empty());
        assert_eq!(Markdown::from_source("").summary(20), "");
    }
}
