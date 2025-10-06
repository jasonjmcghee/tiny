//! Emoji and Nerd Font symbol support
//!
//! JetBrains Mono doesn't include emoji or nerd font glyphs, so we need fallback fonts
//!
//! ## Font Selection Strategy
//!
//! We use a **two-phase approach** for robustness:
//!
//! 1. **Unicode Range Hints** (this file): Fast heuristic to segment text into likely runs
//! 2. **Charmap Verification** (in lib.rs): Actually check if the font has the glyph
//!
//! This way we get good performance (segment once) with correctness (verify before using).
//! The unicode ranges are just hints - we ALWAYS verify with charmap before choosing a font.

/// Check if a character is a Nerd Font symbol (heuristic only!)
///
/// This is a HINT based on unicode ranges. The actual font selection
/// checks the charmap to verify the font actually has the glyph.
///
/// Nerd Fonts primarily use the Private Use Area (PUA) for icons:
/// - Powerline symbols (E0A0-E0D4)
/// - Devicons (E700-E7C5)
/// - Font Awesome (F000-F2E0)
/// - Material Design Icons (F500-FD46)
/// - Weather Icons (E300-E3EB)
/// - Octicons (F400-F4A9)
/// And many more icon sets
pub fn is_nerd_font_symbol(ch: char) -> bool {
    matches!(ch,
        // Private Use Area (PUA) - covers most nerd font symbols
        '\u{E000}'..='\u{F8FF}' |
        // A few common symbols outside PUA
        '\u{2665}' |  // Heart ♥
        '\u{26A1}'    // Lightning bolt ⚡
    )
}

/// Check if a character is an emoji (heuristic only!)
///
/// This is a HINT based on unicode ranges. The actual font selection
/// checks the charmap to verify the font actually has the glyph.
pub fn is_emoji(ch: char) -> bool {
    // Unicode ranges for common emojis
    matches!(ch,
        '\u{1F300}'..='\u{1F6FF}' | // Miscellaneous Symbols, Emoticons, Transport
        '\u{1F900}'..='\u{1F9FF}' | // Supplemental Symbols and Pictographs
        '\u{2600}'..='\u{26FF}'   | // Miscellaneous Symbols
        '\u{2700}'..='\u{27BF}'   | // Dingbats
        '\u{FE00}'..='\u{FE0F}'   | // Variation Selectors
        '\u{1F000}'..='\u{1F02F}' | // Mahjong Tiles
        '\u{1F0A0}'..='\u{1F0FF}' | // Playing Cards
        '\u{1F700}'..='\u{1F77F}' | // Alchemical Symbols
        '\u{1F780}'..='\u{1F7FF}' | // Geometric Shapes Extended
        '\u{1F800}'..='\u{1F8FF}' | // Supplemental Arrows-C
        '\u{1FA00}'..='\u{1FA6F}' | // Chess Symbols + Symbols and Pictographs Extended-A
        '\u{1FA70}'..='\u{1FAFF}'   // Symbols and Pictographs Extended-A
    )
}

/// Detect if text contains emojis
pub fn contains_emoji(text: &str) -> bool {
    text.chars().any(is_emoji)
}

/// Detect if text contains nerd font symbols
pub fn contains_nerd_symbols(text: &str) -> bool {
    text.chars().any(is_nerd_font_symbol)
}

/// Detect if text contains any special characters requiring font fallback (emoji or nerd symbols)
pub fn contains_special_chars(text: &str) -> bool {
    text.chars().any(|ch| is_emoji(ch) || is_nerd_font_symbol(ch))
}

// TODO: Add emoji font loading
// Options:
// 1. Bundle Noto Color Emoji (large, ~10MB)
// 2. Use system emoji font (platform-specific)
//    - macOS: /System/Library/Fonts/Apple Color Emoji.ttc
//    - Windows: C:\Windows\Fonts\seguiemj.ttf
//    - Linux: /usr/share/fonts/truetype/noto/NotoColorEmoji.ttf
// 3. Lazy load on first emoji detection

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_emoji_detection() {
        assert!(is_emoji('👍'));
        assert!(is_emoji('🎉'));
        assert!(is_emoji('🚀'));
        assert!(!is_emoji('a'));
        assert!(!is_emoji('='));
    }

    #[test]
    fn test_contains_emoji() {
        assert!(contains_emoji("Hello 👍 World"));
        assert!(!contains_emoji("Hello World"));
    }
}
