//! Labels, and the one place GitHub dictates colour.
//!
//! See `spec/10-domain-model.md` §3.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    /// Parse `rrggbb` or `#rrggbb`, as GitHub returns for labels.
    pub fn parse_hex(s: &str) -> Option<Self> {
        let s = s.strip_prefix('#').unwrap_or(s);
        if s.len() != 6 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let n = u32::from_str_radix(s, 16).ok()?;
        Some(Self {
            r: (n >> 16) as u8,
            g: (n >> 8) as u8,
            b: n as u8,
        })
    }

    /// WCAG relative luminance, 0.0 (black) to 1.0 (white).
    pub fn relative_luminance(self) -> f32 {
        fn channel(c: u8) -> f32 {
            let c = c as f32 / 255.0;
            if c <= 0.039_285_71 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        }
        0.2126 * channel(self.r) + 0.7152 * channel(self.g) + 0.0722 * channel(self.b)
    }

    /// Whether text drawn on this colour should be light or dark.
    ///
    /// Decided once here rather than at each call site, since a label is the
    /// only element whose colour omaghy does not control — a dark label with
    /// dark text is unreadable regardless of the terminal theme.
    pub fn prefers_light_text(self) -> bool {
        self.relative_luminance() < 0.179
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Label {
    pub name: String,
    pub color: Rgb,
    pub description: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hex_with_and_without_hash() {
        assert_eq!(
            Rgb::parse_hex("d73a4a"),
            Some(Rgb {
                r: 0xd7,
                g: 0x3a,
                b: 0x4a
            })
        );
        assert_eq!(
            Rgb::parse_hex("#0075ca"),
            Some(Rgb {
                r: 0x00,
                g: 0x75,
                b: 0xca
            })
        );
        assert_eq!(Rgb::parse_hex("000000"), Some(Rgb { r: 0, g: 0, b: 0 }));
    }

    #[test]
    fn rejects_malformed_hex() {
        for s in ["", "fff", "gggggg", "d73a4a1", "#"] {
            assert_eq!(Rgb::parse_hex(s), None, "should reject {s:?}");
        }
    }

    #[test]
    fn picks_readable_text_for_real_github_label_colours() {
        // GitHub's own defaults.
        assert!(Rgb::parse_hex("0075ca").unwrap().prefers_light_text()); // documentation, dark blue
        assert!(Rgb::parse_hex("000000").unwrap().prefers_light_text());
        assert!(!Rgb::parse_hex("d4c5f9").unwrap().prefers_light_text()); // pale purple
        assert!(!Rgb::parse_hex("ffffff").unwrap().prefers_light_text());
        assert!(!Rgb::parse_hex("fbca04").unwrap().prefers_light_text()); // yellow
    }
}
