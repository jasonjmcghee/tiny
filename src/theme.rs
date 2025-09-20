//! Theme management for syntax highlighting
//!
//! Themes define colors for each token type and can have multiple colors per token
//! for effects like rainbow variables or animated gradients.

/// A theme defining colors for syntax highlighting
#[derive(Clone, Debug)]
pub struct Theme {
    pub name: String,
    /// Colors per token type (up to 256 token types)
    /// Each token can have multiple colors for effects
    pub token_colors: Vec<TokenColors>,
    /// Maximum number of colors any token has in this theme
    pub max_colors_per_token: usize,
}

/// Colors for a single token type
#[derive(Clone, Debug)]
pub struct TokenColors {
    /// RGBA colors (can be 1 or more for effects)
    pub colors: Vec<[f32; 4]>,
}

impl Theme {
    /// Create a new theme with single colors per token
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            token_colors: Vec::new(),
            max_colors_per_token: 1,
        }
    }

    /// Set colors for a token ID
    pub fn set_token_colors(&mut self, token_id: u8, colors: Vec<[f32; 4]>) {
        // Ensure we have enough slots
        while self.token_colors.len() <= token_id as usize {
            self.token_colors.push(TokenColors {
                colors: vec![[1.0, 1.0, 1.0, 1.0]], // Default white
            });
        }

        self.max_colors_per_token = self.max_colors_per_token.max(colors.len());
        self.token_colors[token_id as usize] = TokenColors { colors };
    }

    /// Generate texture data for GPU (256 x max_colors_per_token)
    /// Each row is a token type, each column is a color variant
    pub fn generate_texture_data(&self) -> Vec<u8> {
        let width = 256;
        let height = self.max_colors_per_token.max(1);
        let mut data = vec![255u8; width * height * 4]; // RGBA8

        for (token_id, token_colors) in self.token_colors.iter().enumerate() {
            for (color_idx, color) in token_colors.colors.iter().enumerate() {
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
            let last_color = token_colors.colors.last().unwrap_or(&[1.0, 1.0, 1.0, 1.0]);
            for color_idx in token_colors.colors.len()..height {
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
        for (token_id, token_colors) in theme1.token_colors.iter().enumerate() {
            for (color_idx, color) in token_colors.colors.iter().enumerate() {
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
        for (token_id, token_colors) in theme2.token_colors.iter().enumerate() {
            for (color_idx, color) in token_colors.colors.iter().enumerate() {
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

        // Basic tokens (1-14) - Original One Dark colors
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
        theme.set_token_colors(15, vec![[0.380, 0.686, 0.937, 1.0]]); // Method - blue (like function)
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
}

