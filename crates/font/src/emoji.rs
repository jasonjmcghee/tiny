//! Emoji support and fallback font handling
//!
//! JetBrains Mono doesn't include emoji glyphs, so we need a fallback system

/// Check if a character is an emoji
pub fn is_emoji(ch: char) -> bool {
    // Simple emoji detection using Unicode ranges
    // This covers most common emojis
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
