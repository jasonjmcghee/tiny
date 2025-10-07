//! Theme management for syntax highlighting
//!
//! Themes define colors for each token type and can have multiple colors per token
//! for effects like rainbow variables or animated gradients.

/// A theme defining colors and styles for syntax highlighting
#[derive(Clone, Debug)]
pub struct Theme {
    pub name: String,
    /// Styles per token type (up to 256 token types)
    /// Each token can have colors and optional style overrides
    pub token_styles: Vec<TokenStyle>,
    /// Maximum number of colors any token has in this theme
    pub max_colors_per_token: usize,
    /// Language-specific style overrides (language name → token overrides)
    pub language_overrides: std::collections::HashMap<String, Vec<(u8, TokenStyle)>>,
}

/// Style attributes for a single token type
#[derive(Clone, Debug)]
pub struct TokenStyle {
    /// RGBA colors (can be 1 or more for effects)
    pub colors: Vec<[f32; 4]>,
    /// Font weight override (None = use default, 100-900 where 400=normal, 700=bold)
    pub weight: Option<f32>,
    /// Italic override (None = use default)
    pub italic: Option<bool>,
    /// Underline decoration
    pub underline: bool,
    /// Strikethrough decoration
    pub strikethrough: bool,
}

/// Colors for a single token type (backward compatibility)
#[derive(Clone, Debug)]
pub struct TokenColors {
    /// RGBA colors (can be 1 or more for effects)
    pub colors: Vec<[f32; 4]>,
}

impl From<TokenColors> for TokenStyle {
    fn from(colors: TokenColors) -> Self {
        Self {
            colors: colors.colors,
            weight: None,
            italic: None,
            underline: false,
            strikethrough: false,
        }
    }
}

impl Theme {
    /// Create a new theme with single colors per token
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            token_styles: Vec::new(),
            max_colors_per_token: 1,
            language_overrides: std::collections::HashMap::new(),
        }
    }

    /// Add language-specific style override for a token
    pub fn add_language_override(&mut self, language: &str, token_id: u8, style: TokenStyle) {
        self.language_overrides
            .entry(language.to_string())
            .or_insert_with(Vec::new)
            .push((token_id, style));
    }

    /// Get token style with language-specific overrides applied
    pub fn get_token_style_for_language(&self, token_id: u8, language: Option<&str>) -> Option<&TokenStyle> {
        // Check for language-specific override first
        if let Some(lang) = language {
            if let Some(overrides) = self.language_overrides.get(lang) {
                for (override_token_id, override_style) in overrides {
                    if *override_token_id == token_id {
                        return Some(override_style);
                    }
                }
            }
        }

        // Fall back to base theme
        self.get_token_style(token_id)
    }

    /// Set full style for a token ID (colors + weight + italic + decorations)
    pub fn set_token_style(&mut self, token_id: u8, style: TokenStyle) {
        // Ensure we have enough slots
        while self.token_styles.len() <= token_id as usize {
            self.token_styles.push(TokenStyle {
                colors: vec![[0.882, 0.882, 0.882, 1.0]], // Default white
                weight: None,
                italic: None,
                underline: false,
                strikethrough: false,
            });
        }

        self.max_colors_per_token = self.max_colors_per_token.max(style.colors.len());
        self.token_styles[token_id as usize] = style;
    }

    /// Set colors for a token ID (backward compatible - no style overrides)
    pub fn set_token_colors(&mut self, token_id: u8, colors: Vec<[f32; 4]>) {
        self.set_token_style(token_id, TokenStyle {
            colors,
            weight: None,
            italic: None,
            underline: false,
            strikethrough: false,
        });
    }

    /// Get style for a token ID
    pub fn get_token_style(&self, token_id: u8) -> Option<&TokenStyle> {
        self.token_styles.get(token_id as usize)
    }

    /// Get colors for a token ID (backward compatible accessor)
    pub fn get_token_colors(&self, token_id: u8) -> Option<&Vec<[f32; 4]>> {
        self.token_styles.get(token_id as usize).map(|s| &s.colors)
    }

    /// Generate texture data for GPU (256 x max_colors_per_token)
    /// Each row is a token type, each column is a color variant
    pub fn generate_texture_data(&self) -> Vec<u8> {
        let width = 256;
        let height = self.max_colors_per_token.max(1);
        let mut data = vec![255u8; width * height * 4]; // RGBA8

        for (token_id, token_style) in self.token_styles.iter().enumerate() {
            for (color_idx, color) in token_style.colors.iter().enumerate() {
                if color_idx >= height {
                    break;
                }

                let pixel_idx = (color_idx * width + token_id) * 4;
                data[pixel_idx] = (color[0] * 255.0) as u8; // R
                data[pixel_idx + 1] = (color[1] * 255.0) as u8; // G
                data[pixel_idx + 2] = (color[2] * 255.0) as u8; // B
                data[pixel_idx + 3] = (color[3] * 255.0) as u8; // A
            }

            // Fill remaining color slots with the last color (for clamping)
            let last_color = token_style.colors.last().unwrap_or(&[1.0, 1.0, 1.0, 1.0]);
            for color_idx in token_style.colors.len()..height {
                let pixel_idx = (color_idx * width + token_id) * 4;
                data[pixel_idx] = (last_color[0] * 255.0) as u8;
                data[pixel_idx + 1] = (last_color[1] * 255.0) as u8;
                data[pixel_idx + 2] = (last_color[2] * 255.0) as u8;
                data[pixel_idx + 3] = (last_color[3] * 255.0) as u8;
            }
        }

        data
    }

    /// Merge two themes for interpolation (stacks them vertically)
    pub fn merge_for_interpolation(theme1: &Theme, theme2: &Theme) -> Vec<u8> {
        let width = 256;
        let max_colors = theme1
            .max_colors_per_token
            .max(theme2.max_colors_per_token)
            .max(1);
        let height = max_colors * 2; // Stack two themes
        let mut data = vec![255u8; width * height * 4];

        // First theme in top half
        for (token_id, token_style) in theme1.token_styles.iter().enumerate() {
            for (color_idx, color) in token_style.colors.iter().enumerate() {
                if color_idx >= max_colors {
                    break;
                }

                let pixel_idx = (color_idx * width + token_id) * 4;
                data[pixel_idx] = (color[0] * 255.0) as u8;
                data[pixel_idx + 1] = (color[1] * 255.0) as u8;
                data[pixel_idx + 2] = (color[2] * 255.0) as u8;
                data[pixel_idx + 3] = (color[3] * 255.0) as u8;
            }
        }

        // Second theme in bottom half
        for (token_id, token_style) in theme2.token_styles.iter().enumerate() {
            for (color_idx, color) in token_style.colors.iter().enumerate() {
                if color_idx >= max_colors {
                    break;
                }

                let pixel_idx = ((color_idx + max_colors) * width + token_id) * 4;
                data[pixel_idx] = (color[0] * 255.0) as u8;
                data[pixel_idx + 1] = (color[1] * 255.0) as u8;
                data[pixel_idx + 2] = (color[2] * 255.0) as u8;
                data[pixel_idx + 3] = (color[3] * 255.0) as u8;
            }
        }

        data
    }
}

/// Built-in themes
pub struct Themes;

impl Themes {
    /// One Dark theme (like GitHub/JetBrains dark)
    pub fn one_dark() -> Theme {
        let mut theme = Theme::new("One Dark");

        // Basic tokens (1-14) - Original One Dark colors (NO style overrides in base)
        theme.set_token_colors(1, vec![[0.776, 0.471, 0.867, 1.0]]); // Keyword - purple
        theme.set_token_colors(2, vec![[0.380, 0.686, 0.937, 1.0]]); // Function - blue
        theme.set_token_colors(3, vec![[0.898, 0.753, 0.482, 1.0]]); // Type - yellow-orange
        theme.set_token_colors(4, vec![[0.596, 0.765, 0.475, 1.0]]); // String - green
        theme.set_token_colors(5, vec![[0.820, 0.604, 0.400, 1.0]]); // Number - orange
        theme.set_token_colors(6, vec![[0.361, 0.388, 0.439, 1.0]]); // Comment - gray
        theme.set_token_colors(7, vec![[0.820, 0.604, 0.400, 1.0]]); // Constant - orange
        theme.set_token_colors(8, vec![[0.337, 0.714, 0.761, 1.0]]); // Operator - cyan
        theme.set_token_colors(9, vec![[0.671, 0.698, 0.749, 1.0]]); // Punctuation - light gray
        theme.set_token_colors(10, vec![[0.671, 0.698, 0.749, 1.0]]); // Variable - light gray
        theme.set_token_colors(11, vec![[0.878, 0.424, 0.459, 1.0]]); // Attribute - red
        theme.set_token_colors(12, vec![[0.380, 0.686, 0.937, 1.0]]); // Namespace - blue
        theme.set_token_colors(13, vec![[0.898, 0.753, 0.482, 1.0]]); // Property - yellow
        theme.set_token_colors(14, vec![[0.671, 0.698, 0.749, 1.0]]); // Parameter - light gray

        // Extended tokens (15+) - Rich syntax highlighting
        theme.set_token_colors(15, vec![[0.380, 0.686, 0.937, 1.0]]); // Method - blue
        theme.set_token_colors(16, vec![[0.898, 0.753, 0.482, 1.0]]); // Field - yellow (like property)
        theme.set_token_colors(17, vec![[0.380, 0.686, 0.937, 1.0]]); // Constructor - blue
        theme.set_token_colors(18, vec![[0.898, 0.753, 0.482, 1.0]]); // Enum - yellow
        theme.set_token_colors(19, vec![[0.820, 0.604, 0.400, 1.0]]); // EnumMember - orange
        theme.set_token_colors(20, vec![[0.898, 0.753, 0.482, 1.0]]); // Interface - yellow
        theme.set_token_colors(21, vec![[0.898, 0.753, 0.482, 1.0]]); // Struct - yellow
        theme.set_token_colors(22, vec![[0.898, 0.753, 0.482, 1.0]]); // Class - yellow
        theme.set_token_colors(23, vec![[0.380, 0.686, 0.937, 1.0]]); // Module - blue
        theme.set_token_colors(24, vec![[0.776, 0.471, 0.867, 1.0]]); // Macro - purple
        theme.set_token_colors(25, vec![[0.898, 0.753, 0.482, 1.0]]); // Label - yellow
        theme.set_token_colors(26, vec![[0.776, 0.471, 0.867, 1.0]]); // KeywordControl - purple

        // String variants
        theme.set_token_colors(27, vec![[0.337, 0.714, 0.761, 1.0]]); // StringEscape - cyan
        theme.set_token_colors(28, vec![[0.380, 0.686, 0.937, 1.0]]); // StringInterpolation - blue
        theme.set_token_colors(29, vec![[0.878, 0.424, 0.459, 1.0]]); // Regex - red

        // Literal variants
        theme.set_token_colors(30, vec![[0.820, 0.604, 0.400, 1.0]]); // Boolean - orange
        theme.set_token_colors(31, vec![[0.596, 0.765, 0.475, 1.0]]); // Character - green
        theme.set_token_colors(32, vec![[0.820, 0.604, 0.400, 1.0]]); // Float - orange

        // Comment variants
        theme.set_token_colors(33, vec![[0.451, 0.478, 0.529, 1.0]]); // CommentDoc - brighter gray
        theme.set_token_colors(34, vec![[0.878, 0.424, 0.459, 1.0]]); // CommentTodo - red

        // Operator variants
        theme.set_token_colors(35, vec![[0.776, 0.471, 0.867, 1.0]]); // ComparisonOp - purple
        theme.set_token_colors(36, vec![[0.776, 0.471, 0.867, 1.0]]); // LogicalOp - purple
        theme.set_token_colors(37, vec![[0.337, 0.714, 0.761, 1.0]]); // ArithmeticOp - cyan

        // Punctuation variants
        theme.set_token_colors(38, vec![[0.671, 0.698, 0.749, 1.0]]); // Bracket - light gray
        theme.set_token_colors(39, vec![[0.671, 0.698, 0.749, 1.0]]); // Brace - light gray
        theme.set_token_colors(40, vec![[0.671, 0.698, 0.749, 1.0]]); // Parenthesis - light gray
        theme.set_token_colors(41, vec![[0.337, 0.714, 0.761, 1.0]]); // Delimiter - cyan
        theme.set_token_colors(42, vec![[0.671, 0.698, 0.749, 1.0]]); // Semicolon - light gray
        theme.set_token_colors(43, vec![[0.671, 0.698, 0.749, 1.0]]); // Comma - light gray

        // Special highlighting
        theme.set_token_colors(44, vec![[0.878, 0.424, 0.459, 1.0]]); // Error - red
        theme.set_token_colors(45, vec![[0.898, 0.753, 0.482, 1.0]]); // Warning - yellow
        theme.set_token_colors(46, vec![[0.584, 0.584, 0.584, 0.7]]); // Deprecated - muted gray
        theme.set_token_colors(47, vec![[0.584, 0.584, 0.584, 0.5]]); // Unused - very muted

        // Rust-specific semantic tokens
        theme.set_token_colors(48, vec![[0.776, 0.471, 0.867, 1.0]]); // SelfKeyword - purple
        theme.set_token_colors(49, vec![[0.337, 0.714, 0.761, 1.0]]); // Lifetime - cyan
        theme.set_token_colors(50, vec![[0.898, 0.753, 0.482, 1.0]]); // TypeParameter - yellow
        theme.set_token_colors(51, vec![[0.898, 0.753, 0.482, 1.0]]); // Generic - yellow
        theme.set_token_colors(52, vec![[0.380, 0.686, 0.937, 1.0]]); // Trait - blue
        theme.set_token_colors(53, vec![[0.776, 0.471, 0.867, 1.0]]); // Derive - purple

        // Line numbers - dim gray
        theme.set_token_colors(255, vec![[0.3, 0.32, 0.34, 1.0]]); // Line numbers - 40% gray, 80% opacity

        // === MARKDOWN-SPECIFIC STYLE OVERRIDES ===
        // These ONLY apply when rendering markdown files, not Rust/TOML/etc.

        // Token 1 (Keyword): Headings with weight 900 (extra bold)
        theme.add_language_override("markdown", 1, TokenStyle {
            colors: vec![[0.776, 0.471, 0.867, 1.0]], // Purple (same color)
            weight: Some(900.0), // Extra bold for headings
            italic: None,
            underline: false,
            strikethrough: false,
        });

        // Token 10 (Variable): Emphasis with italic
        theme.add_language_override("markdown", 10, TokenStyle {
            colors: vec![[0.671, 0.698, 0.749, 1.0]], // Light gray (same color)
            weight: None,
            italic: Some(true), // Italic for *emphasis*
            underline: false,
            strikethrough: false,
        });

        // Token 15 (Method): Strong with weight 700 (bold)
        theme.add_language_override("markdown", 15, TokenStyle {
            colors: vec![[0.380, 0.686, 0.937, 1.0]], // Blue (same color)
            weight: Some(700.0), // Bold for **strong**
            italic: None,
            underline: false,
            strikethrough: false,
        });

        theme
    }

    /// Monokai theme
    pub fn monokai() -> Theme {
        let mut theme = Theme::new("Monokai");

        // Basic tokens (1-14) - Classic Monokai colors
        theme.set_token_colors(1, vec![[0.976, 0.149, 0.447, 1.0]]); // Keyword - pink (#F92672)
        theme.set_token_colors(2, vec![[0.651, 0.886, 0.180, 1.0]]); // Function - green (#A6E22E)
        theme.set_token_colors(3, vec![[0.400, 0.851, 0.937, 1.0]]); // Type - cyan (#66D9EF)
        theme.set_token_colors(4, vec![[0.902, 0.859, 0.455, 1.0]]); // String - yellow (#E6DB74)
        theme.set_token_colors(5, vec![[0.682, 0.506, 0.976, 1.0]]); // Number - purple (#AE81FF)
        theme.set_token_colors(6, vec![[0.459, 0.443, 0.369, 1.0]]); // Comment - brown-gray (#75715E)
        theme.set_token_colors(7, vec![[0.682, 0.506, 0.976, 1.0]]); // Constant - purple (#AE81FF)
        theme.set_token_colors(8, vec![[0.976, 0.149, 0.447, 1.0]]); // Operator - pink (#F92672)
        theme.set_token_colors(9, vec![[0.972, 0.972, 0.949, 1.0]]); // Punctuation - light gray (#F8F8F2)
        theme.set_token_colors(10, vec![[0.972, 0.972, 0.949, 1.0]]); // Variable - light gray (#F8F8F2)
        theme.set_token_colors(11, vec![[0.651, 0.886, 0.180, 1.0]]); // Attribute - green (#A6E22E)
        theme.set_token_colors(12, vec![[0.400, 0.851, 0.937, 1.0]]); // Namespace - cyan (#66D9EF)
        theme.set_token_colors(13, vec![[0.902, 0.859, 0.455, 1.0]]); // Property - yellow (#E6DB74)
        theme.set_token_colors(14, vec![[0.992, 0.592, 0.122, 1.0]]); // Parameter - orange (#FD971F)

        // Extended tokens (15+) - Rich Monokai palette
        theme.set_token_colors(15, vec![[0.651, 0.886, 0.180, 1.0]]); // Method - green (like function)
        theme.set_token_colors(16, vec![[0.902, 0.859, 0.455, 1.0]]); // Field - yellow (like property)
        theme.set_token_colors(17, vec![[0.651, 0.886, 0.180, 1.0]]); // Constructor - green
        theme.set_token_colors(18, vec![[0.400, 0.851, 0.937, 1.0]]); // Enum - cyan
        theme.set_token_colors(19, vec![[0.682, 0.506, 0.976, 1.0]]); // EnumMember - purple
        theme.set_token_colors(20, vec![[0.400, 0.851, 0.937, 1.0]]); // Interface - cyan
        theme.set_token_colors(21, vec![[0.400, 0.851, 0.937, 1.0]]); // Struct - cyan
        theme.set_token_colors(22, vec![[0.400, 0.851, 0.937, 1.0]]); // Class - cyan
        theme.set_token_colors(23, vec![[0.400, 0.851, 0.937, 1.0]]); // Module - cyan
        theme.set_token_colors(24, vec![[0.976, 0.149, 0.447, 1.0]]); // Macro - pink
        theme.set_token_colors(25, vec![[0.902, 0.859, 0.455, 1.0]]); // Label - yellow
        theme.set_token_colors(26, vec![[0.976, 0.149, 0.447, 1.0]]); // KeywordControl - pink

        // String variants
        theme.set_token_colors(27, vec![[0.992, 0.592, 0.122, 1.0]]); // StringEscape - orange
        theme.set_token_colors(28, vec![[0.651, 0.886, 0.180, 1.0]]); // StringInterpolation - green
        theme.set_token_colors(29, vec![[0.976, 0.149, 0.447, 1.0]]); // Regex - pink

        // Literal variants
        theme.set_token_colors(30, vec![[0.682, 0.506, 0.976, 1.0]]); // Boolean - purple
        theme.set_token_colors(31, vec![[0.902, 0.859, 0.455, 1.0]]); // Character - yellow
        theme.set_token_colors(32, vec![[0.682, 0.506, 0.976, 1.0]]); // Float - purple

        // Comment variants
        theme.set_token_colors(33, vec![[0.549, 0.525, 0.459, 1.0]]); // CommentDoc - lighter brown
        theme.set_token_colors(34, vec![[0.976, 0.149, 0.447, 1.0]]); // CommentTodo - pink

        // Operator variants
        theme.set_token_colors(35, vec![[0.976, 0.149, 0.447, 1.0]]); // ComparisonOp - pink
        theme.set_token_colors(36, vec![[0.976, 0.149, 0.447, 1.0]]); // LogicalOp - pink
        theme.set_token_colors(37, vec![[0.976, 0.149, 0.447, 1.0]]); // ArithmeticOp - pink

        // Punctuation variants
        theme.set_token_colors(38, vec![[0.972, 0.972, 0.949, 1.0]]); // Bracket - light gray
        theme.set_token_colors(39, vec![[0.972, 0.972, 0.949, 1.0]]); // Brace - light gray
        theme.set_token_colors(40, vec![[0.972, 0.972, 0.949, 1.0]]); // Parenthesis - light gray
        theme.set_token_colors(41, vec![[0.976, 0.149, 0.447, 1.0]]); // Delimiter - pink
        theme.set_token_colors(42, vec![[0.972, 0.972, 0.949, 1.0]]); // Semicolon - light gray
        theme.set_token_colors(43, vec![[0.972, 0.972, 0.949, 1.0]]); // Comma - light gray

        // Special highlighting
        theme.set_token_colors(44, vec![[0.976, 0.149, 0.447, 1.0]]); // Error - pink
        theme.set_token_colors(45, vec![[0.902, 0.859, 0.455, 1.0]]); // Warning - yellow
        theme.set_token_colors(46, vec![[0.584, 0.584, 0.584, 0.7]]); // Deprecated - muted gray
        theme.set_token_colors(47, vec![[0.584, 0.584, 0.584, 0.5]]); // Unused - very muted

        // Rust-specific semantic tokens
        theme.set_token_colors(48, vec![[0.976, 0.149, 0.447, 1.0]]); // SelfKeyword - pink
        theme.set_token_colors(49, vec![[0.992, 0.592, 0.122, 1.0]]); // Lifetime - orange
        theme.set_token_colors(50, vec![[0.400, 0.851, 0.937, 1.0]]); // TypeParameter - cyan
        theme.set_token_colors(51, vec![[0.400, 0.851, 0.937, 1.0]]); // Generic - cyan
        theme.set_token_colors(52, vec![[0.651, 0.886, 0.180, 1.0]]); // Trait - green
        theme.set_token_colors(53, vec![[0.976, 0.149, 0.447, 1.0]]); // Derive - pink

        // Line numbers - dim gray (Monokai style)
        theme.set_token_colors(255, vec![[0.459, 0.443, 0.369, 0.8]]); // Line numbers - brown-gray, 80% opacity

        theme
    }

    /// Rainbow theme - multiple colors per token for effects
    pub fn rainbow() -> Theme {
        let mut theme = Theme::new("Rainbow");

        // Keywords cycle through purple shades
        theme.set_token_colors(
            1,
            vec![
                [0.776, 0.471, 0.867, 1.0],
                [0.600, 0.400, 0.800, 1.0],
                [0.867, 0.471, 0.776, 1.0],
            ],
        );

        // Variables cycle through rainbow
        theme.set_token_colors(
            10,
            vec![
                [1.0, 0.0, 0.0, 1.0], // Red
                [1.0, 0.5, 0.0, 1.0], // Orange
                [1.0, 1.0, 0.0, 1.0], // Yellow
                [0.0, 1.0, 0.0, 1.0], // Green
                [0.0, 0.0, 1.0, 1.0], // Blue
                [0.5, 0.0, 1.0, 1.0], // Purple
            ],
        );

        // Other tokens use single colors from one_dark
        theme.set_token_colors(2, vec![[0.380, 0.686, 0.937, 1.0]]); // Function
        theme.set_token_colors(3, vec![[0.898, 0.753, 0.482, 1.0]]); // Type
        theme.set_token_colors(4, vec![[0.596, 0.765, 0.475, 1.0]]); // String
        theme.set_token_colors(5, vec![[0.820, 0.604, 0.400, 1.0]]); // Number
        theme.set_token_colors(6, vec![[0.361, 0.388, 0.439, 1.0]]); // Comment
        theme.set_token_colors(7, vec![[0.820, 0.604, 0.400, 1.0]]); // Constant
        theme.set_token_colors(8, vec![[0.337, 0.714, 0.761, 1.0]]); // Operator
        theme.set_token_colors(9, vec![[0.671, 0.698, 0.749, 1.0]]); // Punctuation
        theme.set_token_colors(11, vec![[0.878, 0.424, 0.459, 1.0]]); // Attribute
        theme.set_token_colors(12, vec![[0.380, 0.686, 0.937, 1.0]]); // Namespace
        theme.set_token_colors(13, vec![[0.898, 0.753, 0.482, 1.0]]); // Property
        theme.set_token_colors(14, vec![[0.671, 0.698, 0.749, 1.0]]); // Parameter

        theme
    }

    /// Demonstrative theme - showcases all font weights, italics, and decorations
    /// Systematically shows weight progression and style combinations
    pub fn demonstrative() -> Theme {
        let mut theme = Theme::new("Demonstrative");

        // Helper: Generate a distinct color for each token based on hue rotation
        let color_for_token = |token_id: u8| -> [f32; 4] {
            let hue = (token_id as f32 * 13.7) % 360.0; // Prime number for good distribution
            let (r, g, b) = hsl_to_rgb(hue, 0.8, 0.6);
            [r, g, b, 1.0]
        };

        // Tokens 1-9: Regular weight progression (100-900)
        for (i, weight) in [100.0, 200.0, 300.0, 400.0, 500.0, 600.0, 700.0, 800.0, 900.0].iter().enumerate() {
            let token_id = (i + 1) as u8;
            theme.set_token_style(token_id, TokenStyle {
                colors: vec![color_for_token(token_id)],
                weight: Some(*weight),
                italic: None,
                underline: false,
                strikethrough: false,
            });
        }

        // Tokens 10-18: Italic weight progression (100-900)
        for (i, weight) in [100.0, 200.0, 300.0, 400.0, 500.0, 600.0, 700.0, 800.0, 900.0].iter().enumerate() {
            let token_id = (i + 10) as u8;
            theme.set_token_style(token_id, TokenStyle {
                colors: vec![color_for_token(token_id)],
                weight: Some(*weight),
                italic: Some(true),
                underline: false,
                strikethrough: false,
            });
        }

        // Token 19: Regular weight 400, underline
        theme.set_token_style(19, TokenStyle {
            colors: vec![[0.2, 0.8, 1.0, 1.0]], // Bright cyan
            weight: Some(400.0),
            italic: None,
            underline: true,
            strikethrough: false,
        });

        // Token 20: Regular weight 400, strikethrough
        theme.set_token_style(20, TokenStyle {
            colors: vec![[1.0, 0.5, 0.2, 1.0]], // Orange
            weight: Some(400.0),
            italic: None,
            underline: false,
            strikethrough: true,
        });

        // Token 21: Bold 700, italic, underline
        theme.set_token_style(21, TokenStyle {
            colors: vec![[0.9, 0.2, 0.9, 1.0]], // Magenta
            weight: Some(700.0),
            italic: Some(true),
            underline: true,
            strikethrough: false,
        });

        // Token 22: Bold 700, italic, strikethrough
        theme.set_token_style(22, TokenStyle {
            colors: vec![[0.2, 0.9, 0.3, 1.0]], // Green
            weight: Some(700.0),
            italic: Some(true),
            underline: false,
            strikethrough: true,
        });

        // Token 23: Bold 700, italic, underline + strikethrough
        theme.set_token_style(23, TokenStyle {
            colors: vec![[0.9, 0.9, 0.2, 1.0]], // Yellow
            weight: Some(700.0),
            italic: Some(true),
            underline: true,
            strikethrough: true,
        });

        // Token 24: Thin 100, italic, underline
        theme.set_token_style(24, TokenStyle {
            colors: vec![[1.0, 0.4, 0.7, 1.0]], // Pink
            weight: Some(100.0),
            italic: Some(true),
            underline: true,
            strikethrough: false,
        });

        // Token 25: Extra-bold 900, underline
        theme.set_token_style(25, TokenStyle {
            colors: vec![[0.5, 0.3, 0.9, 1.0]], // Purple
            weight: Some(900.0),
            italic: None,
            underline: true,
            strikethrough: false,
        });

        theme
    }

    /// Showcase theme - demonstrates ALL styling features with actual syntax tokens
    /// Uses variety of weights, italics, underline, strikethrough, multi-colors, etc.
    pub fn showcase() -> Theme {
        let mut theme = Theme::new("Showcase");

        // === KEYWORDS (token 1) ===
        // Bold 700, purple gradient (2 colors for shine/rainbow effect)
        theme.set_token_style(1, TokenStyle {
            colors: vec![
                [0.776, 0.471, 0.867, 1.0], // Purple
                [0.867, 0.471, 0.776, 1.0], // Pink-purple
            ],
            weight: Some(700.0), // Bold
            italic: None,
            underline: false,
            strikethrough: false,
        });

        // === FUNCTION (token 2) ===
        // Weight 600, italic, blue with cyan gradient (3 colors)
        theme.set_token_style(2, TokenStyle {
            colors: vec![
                [0.380, 0.686, 0.937, 1.0], // Blue
                [0.337, 0.714, 0.761, 1.0], // Cyan
                [0.400, 0.851, 0.937, 1.0], // Light blue
            ],
            weight: Some(600.0),
            italic: Some(true), // Italic functions
            underline: false,
            strikethrough: false,
        });

        // === TYPE (token 3) ===
        // Weight 500, yellow-orange gradient
        theme.set_token_style(3, TokenStyle {
            colors: vec![
                [0.898, 0.753, 0.482, 1.0], // Yellow
                [0.992, 0.592, 0.122, 1.0], // Orange
            ],
            weight: Some(500.0),
            italic: None,
            underline: false,
            strikethrough: false,
        });

        // === STRING (token 4) ===
        // Weight 400, green gradient (4 colors for rainbow effect)
        theme.set_token_style(4, TokenStyle {
            colors: vec![
                [0.596, 0.765, 0.475, 1.0], // Green
                [0.651, 0.886, 0.180, 1.0], // Bright green
                [0.400, 0.851, 0.937, 1.0], // Cyan
                [0.596, 0.765, 0.475, 1.0], // Back to green
            ],
            weight: None,
            italic: None,
            underline: false,
            strikethrough: false,
        });

        // === NUMBER (token 5) ===
        // Weight 800, orange gradient
        theme.set_token_style(5, TokenStyle {
            colors: vec![
                [0.820, 0.604, 0.400, 1.0], // Orange
                [0.992, 0.592, 0.122, 1.0], // Bright orange
            ],
            weight: Some(800.0),
            italic: None,
            underline: false,
            strikethrough: false,
        });

        // === COMMENT (token 6) ===
        // Weight 300, italic, dim gray gradient
        theme.set_token_style(6, TokenStyle {
            colors: vec![
                [0.361, 0.388, 0.439, 1.0], // Gray
                [0.451, 0.478, 0.529, 1.0], // Lighter gray
            ],
            weight: Some(300.0), // Thin
            italic: Some(true),  // Italic comments
            underline: false,
            strikethrough: false,
        });

        // === CONSTANT (token 7) ===
        // Weight 900, underlined, bright purple gradient
        theme.set_token_style(7, TokenStyle {
            colors: vec![
                [0.682, 0.506, 0.976, 1.0], // Purple
                [0.776, 0.471, 0.867, 1.0], // Magenta
            ],
            weight: Some(900.0), // Extra bold
            italic: None,
            underline: true, // Underlined constants
            strikethrough: false,
        });

        // === OPERATOR (token 8) ===
        // Weight 600, cyan-pink rainbow (5 colors!)
        theme.set_token_style(8, TokenStyle {
            colors: vec![
                [0.337, 0.714, 0.761, 1.0], // Cyan
                [0.380, 0.686, 0.937, 1.0], // Blue
                [0.776, 0.471, 0.867, 1.0], // Purple
                [0.976, 0.149, 0.447, 1.0], // Pink
                [0.337, 0.714, 0.761, 1.0], // Back to cyan
            ],
            weight: Some(600.0),
            italic: None,
            underline: false,
            strikethrough: false,
        });

        // === PUNCTUATION (token 9) ===
        // Weight 400, light gray
        theme.set_token_style(9, TokenStyle {
            colors: vec![[0.671, 0.698, 0.749, 1.0]],
            weight: None,
            italic: None,
            underline: false,
            strikethrough: false,
        });

        // === VARIABLE (token 10) ===
        // Weight 400, full rainbow spectrum (6 colors)
        theme.set_token_style(10, TokenStyle {
            colors: vec![
                [1.0, 0.3, 0.3, 1.0], // Red
                [1.0, 0.6, 0.2, 1.0], // Orange
                [0.9, 0.9, 0.3, 1.0], // Yellow
                [0.3, 0.9, 0.4, 1.0], // Green
                [0.3, 0.6, 1.0, 1.0], // Blue
                [0.7, 0.4, 1.0, 1.0], // Purple
            ],
            weight: None,
            italic: None,
            underline: false,
            strikethrough: false,
        });

        // === ATTRIBUTE (token 11) ===
        // Weight 500, italic, underlined, red-orange gradient
        theme.set_token_style(11, TokenStyle {
            colors: vec![
                [0.878, 0.424, 0.459, 1.0], // Red
                [0.992, 0.592, 0.122, 1.0], // Orange
            ],
            weight: Some(500.0),
            italic: Some(true),
            underline: true, // Underlined attributes
            strikethrough: false,
        });

        // === NAMESPACE (token 12) ===
        // Weight 700, blue
        theme.set_token_style(12, TokenStyle {
            colors: vec![[0.380, 0.686, 0.937, 1.0]],
            weight: Some(700.0),
            italic: None,
            underline: false,
            strikethrough: false,
        });

        // === PROPERTY (token 13) ===
        // Weight 400, italic, yellow gradient
        theme.set_token_style(13, TokenStyle {
            colors: vec![
                [0.898, 0.753, 0.482, 1.0], // Yellow
                [0.902, 0.859, 0.455, 1.0], // Bright yellow
            ],
            weight: None,
            italic: Some(true), // Italic properties
            underline: false,
            strikethrough: false,
        });

        // === PARAMETER (token 14) ===
        // Weight 400, orange gradient
        theme.set_token_style(14, TokenStyle {
            colors: vec![
                [0.992, 0.592, 0.122, 1.0], // Orange
                [0.820, 0.604, 0.400, 1.0], // Muted orange
            ],
            weight: None,
            italic: None,
            underline: false,
            strikethrough: false,
        });

        // === METHOD (token 15) ===
        // Weight 600, italic, blue-cyan gradient
        theme.set_token_style(15, TokenStyle {
            colors: vec![
                [0.380, 0.686, 0.937, 1.0], // Blue
                [0.400, 0.851, 0.937, 1.0], // Cyan
            ],
            weight: Some(600.0),
            italic: Some(true),
            underline: false,
            strikethrough: false,
        });

        // === MACRO (token 24) ===
        // Weight 900, underlined + strikethrough (!), pink-purple rainbow
        theme.set_token_style(24, TokenStyle {
            colors: vec![
                [0.976, 0.149, 0.447, 1.0], // Pink
                [0.867, 0.471, 0.776, 1.0], // Magenta
                [0.776, 0.471, 0.867, 1.0], // Purple
            ],
            weight: Some(900.0),
            italic: Some(true),
            underline: true,
            strikethrough: true, // Both decorations!
        });

        // === COMMENT DOC (token 33) ===
        // Weight 400, italic, underlined, brighter gray
        theme.set_token_style(33, TokenStyle {
            colors: vec![[0.451, 0.478, 0.529, 1.0]],
            weight: None,
            italic: Some(true),
            underline: true, // Underlined doc comments
            strikethrough: false,
        });

        // === DEPRECATED (token 46) ===
        // Weight 400, strikethrough, muted gray
        theme.set_token_style(46, TokenStyle {
            colors: vec![[0.584, 0.584, 0.584, 0.7]],
            weight: None,
            italic: None,
            underline: false,
            strikethrough: true, // Strikethrough for deprecated
        });

        // === LIFETIME (token 49) ===
        // Weight 700, italic, cyan-blue gradient
        theme.set_token_style(49, TokenStyle {
            colors: vec![
                [0.337, 0.714, 0.761, 1.0], // Cyan
                [0.380, 0.686, 0.937, 1.0], // Blue
            ],
            weight: Some(700.0),
            italic: Some(true), // Italic lifetimes
            underline: false,
            strikethrough: false,
        });

        // === TRAIT (token 52) ===
        // Weight 700, underlined, bright blue gradient
        theme.set_token_style(52, TokenStyle {
            colors: vec![
                [0.380, 0.686, 0.937, 1.0], // Blue
                [0.400, 0.851, 0.937, 1.0], // Cyan
            ],
            weight: Some(700.0),
            italic: None,
            underline: true, // Underlined traits
            strikethrough: false,
        });

        // === DERIVE (token 53) ===
        // Weight 800, italic + underlined, purple-pink gradient
        theme.set_token_style(53, TokenStyle {
            colors: vec![
                [0.776, 0.471, 0.867, 1.0], // Purple
                [0.976, 0.149, 0.447, 1.0], // Pink
            ],
            weight: Some(800.0),
            italic: Some(true),
            underline: true, // Multiple effects
            strikethrough: false,
        });

        // Line numbers - dim gray
        theme.set_token_style(255, TokenStyle {
            colors: vec![[0.3, 0.32, 0.34, 1.0]],
            weight: Some(300.0), // Thin
            italic: None,
            underline: false,
            strikethrough: false,
        });

        theme
    }
}

/// Convert HSL to RGB (helper for demonstrative theme)
fn hsl_to_rgb(h: f32, s: f32, l: f32) -> (f32, f32, f32) {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = l - c / 2.0;

    let (r, g, b) = if h < 60.0 {
        (c, x, 0.0)
    } else if h < 120.0 {
        (x, c, 0.0)
    } else if h < 180.0 {
        (0.0, c, x)
    } else if h < 240.0 {
        (0.0, x, c)
    } else if h < 300.0 {
        (x, 0.0, c)
    } else {
        (c, 0.0, x)
    };

    (r + m, g + m, b + m)
}
