//! Extensible text styling system for syntax highlighting, spell check, etc.
//!
//! TextStyleProvider trait allows pluggable text effects that integrate
//! seamlessly with the render graph for maximum performance.

use std::ops::Range;

// === Core Trait ===

/// Provider of text styling effects (syntax highlighting, spell check, etc.)
pub trait TextStyleProvider: Send + Sync {
    /// Get all text effects for a byte range - called during rendering
    fn get_effects_in_range(&self, range: Range<usize>) -> Vec<TextEffect>;

    /// Request update for this provider (non-blocking)
    /// Version allows ignoring stale updates
    fn request_update(&self, text: &str, version: u64);

    /// Get provider name for debugging
    fn name(&self) -> &str;

    /// Downcast support for type-specific operations
    fn as_any(&self) -> &dyn std::any::Any;
}

/// A single text styling effect
#[derive(Clone, Debug)]
pub struct TextEffect {
    /// Byte range this effect applies to
    pub range: Range<usize>,
    /// The visual effect to apply
    pub effect: EffectType,
    /// Priority for conflict resolution (higher wins)
    pub priority: u8,
}

/// Types of visual effects that can be applied to text
#[derive(Clone, Debug, PartialEq)]
pub enum EffectType {
    /// Token with ID for theme lookup
    Token(u8),
    /// Custom shader effect with optional parameters
    Shader {
        id: u32,
        params: Option<std::sync::Arc<[f32; 4]>>,
    },
    /// Change font weight
    Weight(FontWeight),
    /// Make text italic
    Italic(bool),
    /// Transform text (scale, rotation, etc.)
    Transform(TextTransform),
    /// Background color (rectangle behind text)
    Background(u32),
    /// Underline with specific style
    Underline { color: u32, style: UnderlineStyle },
    /// Strikethrough
    Strikethrough(u32),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FontWeight {
    Thin = 100,
    Light = 300,
    Regular = 400,
    Medium = 500,
    Bold = 700,
    Black = 900,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TextTransform {
    pub scale_x: f32,
    pub scale_y: f32,
    pub rotation: f32,
    pub offset_x: f32,
    pub offset_y: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum UnderlineStyle {
    Solid,
    Dashed,
    Dotted,
    Wavy, // For spell check errors
    Double,
}

// === Style Registry ===

/// Manages multiple text style providers and resolves conflicts
pub struct StyleRegistry {
    providers: Vec<Box<dyn TextStyleProvider>>,
}

impl StyleRegistry {
    /// Create empty registry
    pub fn new() -> Self {
        Self {
            providers: Vec::new(),
        }
    }

    /// Add a style provider
    pub fn add_provider(&mut self, provider: Box<dyn TextStyleProvider>) {
        self.providers.push(provider);
    }

    /// Get all effects for a range, with conflict resolution
    pub fn get_effects_in_range(&self, range: Range<usize>) -> Vec<TextEffect> {
        let mut all_effects = Vec::new();

        // Collect effects from all providers
        for provider in &self.providers {
            all_effects.extend(provider.get_effects_in_range(range.clone()));
        }

        // Sort by priority (higher priority wins conflicts)
        all_effects.sort_by_key(|e| e.priority);

        // TODO: Resolve overlapping effects based on priority
        // For now, just return all effects
        all_effects
    }

    /// Request update from all providers
    pub fn request_update(&self, text: &str, version: u64) {
        for provider in &self.providers {
            provider.request_update(text, version);
        }
    }

    /// Get provider by name (for debugging)
    pub fn get_provider(&self, name: &str) -> Option<&dyn TextStyleProvider> {
        self.providers
            .iter()
            .find(|p| p.name() == name)
            .map(|p| p.as_ref())
    }
}

// === Built-in Priority Constants ===

pub mod priority {
    /// Base text styling (lowest priority)
    pub const BASE: u8 = 0;
    /// Syntax highlighting
    pub const SYNTAX: u8 = 10;
    /// Search highlighting
    pub const SEARCH: u8 = 20;
    /// Error highlighting
    pub const ERROR: u8 = 30;
    /// Selection highlighting (highest priority)
    pub const SELECTION: u8 = 40;
}

// === Default Implementations ===

impl Default for TextTransform {
    fn default() -> Self {
        Self {
            scale_x: 1.0,
            scale_y: 1.0,
            rotation: 0.0,
            offset_x: 0.0,
            offset_y: 0.0,
        }
    }
}

impl Default for StyleRegistry {
    fn default() -> Self {
        Self::new()
    }
}
