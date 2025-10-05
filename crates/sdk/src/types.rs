//! Shared types used across plugins
//!
//! These types are part of the SDK because multiple plugins need them.
//! We keep this minimal - most types should live in the plugins that own them.

// === Coordinate System Types ===
//
// Four distinct coordinate spaces with explicit transformations:
// 1. Document space: bytes, lines, columns (what editor manipulates)
// 2. Layout space: logical pixels, pre-scroll (where widgets live)
// 3. View space: logical pixels, post-scroll (what's visible)
// 4. Physical space: device pixels (what GPU renders)

// === Logical Pixels (DPI-independent unit) ===

use std::fmt::Display;

use bytemuck::{Pod, Zeroable};

/// Logical pixels - DPI-independent unit used by Layout and View spaces
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Default, Pod, Zeroable)]
pub struct LogicalPixels(pub f32);

impl LogicalPixels {
    pub fn to_physical(self, scale_factor: f32) -> PhysicalPixels {
        PhysicalPixels(self.0 * scale_factor)
    }
}

impl std::ops::Add for LogicalPixels {
    type Output = Self;
    fn add(self, rhs: Self) -> Self {
        LogicalPixels(self.0 + rhs.0)
    }
}

impl std::ops::Add<f32> for LogicalPixels {
    type Output = Self;
    fn add(self, rhs: f32) -> Self {
        LogicalPixels(self.0 + rhs)
    }
}

impl std::ops::Sub for LogicalPixels {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self {
        LogicalPixels(self.0 - rhs.0)
    }
}

impl std::ops::Sub<f32> for LogicalPixels {
    type Output = Self;
    fn sub(self, rhs: f32) -> Self {
        LogicalPixels(self.0 - rhs)
    }
}

impl std::ops::Mul<f32> for LogicalPixels {
    type Output = Self;
    fn mul(self, rhs: f32) -> Self {
        LogicalPixels(self.0 * rhs)
    }
}

impl std::ops::Div<f32> for LogicalPixels {
    type Output = f32;
    fn div(self, rhs: f32) -> f32 {
        self.0 / rhs
    }
}

impl std::fmt::Display for LogicalPixels {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

// === Physical Pixels (device pixels) ===

/// Physical pixels - actual device pixels
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default, Pod, Zeroable)]
pub struct PhysicalPixels(pub f32);

impl PhysicalPixels {
    pub fn to_logical(self, scale_factor: f32) -> LogicalPixels {
        LogicalPixels(self.0 / scale_factor)
    }
}

// === Document Space ===

/// Position in document (text/editing operations)
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Pod, Zeroable)]
pub struct DocPos {
    /// Byte offset in the document
    pub byte_offset: usize,
    /// Line number (0-indexed)
    pub line: u32,
    /// Visual column (0-indexed, accounts for tabs)
    pub column: u32,
}

// === Layout Space (pre-scroll) ===

/// Position in layout space - where things are before scrolling
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default, Pod, Zeroable)]
pub struct LayoutPos {
    pub x: LogicalPixels,
    pub y: LogicalPixels,
}

impl LayoutPos {
    pub fn new(x: f32, y: f32) -> Self {
        Self {
            x: LogicalPixels(x),
            y: LogicalPixels(y),
        }
    }
}

impl std::ops::Add for LayoutPos {
    type Output = Self;
    fn add(self, rhs: Self) -> Self {
        Self {
            x: self.x + rhs.x,
            y: self.y + rhs.y,
        }
    }
}

impl std::ops::Sub for LayoutPos {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self {
        Self {
            x: self.x - rhs.x,
            y: self.y - rhs.y,
        }
    }
}

impl std::ops::Mul<f32> for LayoutPos {
    type Output = Self;
    fn mul(self, rhs: f32) -> Self {
        Self {
            x: self.x * rhs,
            y: self.y * rhs,
        }
    }
}

impl Display for LayoutPos {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "({}, {})", self.x, self.y)
    }
}

/// Size in layout/logical space
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default, Pod, Zeroable)]
pub struct LogicalSize {
    pub width: LogicalPixels,
    pub height: LogicalPixels,
}

impl LogicalSize {
    pub fn new(width: f32, height: f32) -> Self {
        Self {
            width: LogicalPixels(width),
            height: LogicalPixels(height),
        }
    }
}

/// Rectangle in layout space
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default, Pod, Zeroable)]
pub struct LayoutRect {
    pub x: LogicalPixels,
    pub y: LogicalPixels,
    pub width: LogicalPixels,
    pub height: LogicalPixels,
}

impl LayoutRect {
    pub fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x: LogicalPixels(x),
            y: LogicalPixels(y),
            width: LogicalPixels(width),
            height: LogicalPixels(height),
        }
    }

    pub fn contains(&self, pt: LayoutPos) -> bool {
        pt.x.0 >= self.x.0
            && pt.x.0 <= self.x.0 + self.width.0
            && pt.y.0 >= self.y.0
            && pt.y.0 <= self.y.0 + self.height.0
    }
}

// === View Space (post-scroll) ===

/// Position in view space - layout minus scroll offset
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default, Pod, Zeroable)]
pub struct ViewPos {
    pub x: LogicalPixels,
    pub y: LogicalPixels,
}

impl ViewPos {
    pub fn new(x: f32, y: f32) -> Self {
        Self {
            x: LogicalPixels(x),
            y: LogicalPixels(y),
        }
    }
}

/// Rectangle in view space
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default, Pod, Zeroable)]
pub struct ViewRect {
    pub x: LogicalPixels,
    pub y: LogicalPixels,
    pub width: LogicalPixels,
    pub height: LogicalPixels,
}

impl ViewRect {
    pub fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x: LogicalPixels(x),
            y: LogicalPixels(y),
            width: LogicalPixels(width),
            height: LogicalPixels(height),
        }
    }

    pub fn contains(&self, pos: ViewPos) -> bool {
        pos.x >= self.x
            && pos.x <= self.x + self.width
            && pos.y >= self.y
            && pos.y <= self.y + self.height
    }
}

// === Physical Space ===

/// Position in physical pixels
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default, Pod, Zeroable)]
pub struct PhysicalPos {
    pub x: PhysicalPixels,
    pub y: PhysicalPixels,
}

impl PhysicalPos {
    pub fn new(x: f32, y: f32) -> Self {
        Self {
            x: PhysicalPixels(x),
            y: PhysicalPixels(y),
        }
    }
}

/// Size in physical pixels (for GPU)
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default, Pod, Zeroable)]
pub struct PhysicalSize {
    pub width: u32,
    pub height: u32,
}

/// Size in physical pixels (float version for calculations)
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default, Pod, Zeroable)]
pub struct PhysicalSizeF {
    pub width: PhysicalPixels,
    pub height: PhysicalPixels,
}

impl PhysicalSizeF {
    pub fn new(width: f32, height: f32) -> Self {
        Self {
            width: PhysicalPixels(width),
            height: PhysicalPixels(height),
        }
    }
}

// === Legacy compatibility aliases ===
pub type LogicalPos = LayoutPos;
pub type LogicalRect = LayoutRect;

// === Glyph Data (for Hook<GlyphInstances>) ===

/// A batch of glyphs to be rendered
/// This is the main data type that flows through text rendering hooks
#[derive(Debug, Clone)]
pub struct GlyphInstances {
    pub glyphs: Vec<GlyphInstance>,
}

/// Single glyph instance
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct GlyphInstance {
    /// Position in layout space
    pub pos: LayoutPos,
    /// Texture coordinates in font atlas [u0, v0, u1, v1]
    pub tex_coords: [f32; 4],
    /// Relative position within token (0.0-1.0) for effects
    pub relative_pos: f32,
    /// Optional shader effect ID (0 = none)
    pub shader_id: u32,
    /// Token ID for syntax coloring (0 = default)
    pub token_id: u8,
    /// Format flags for visual effects (0x01 = half-opacity, 0x02 = underline, 0x04 = highlight)
    pub format: u8,
    /// Padding for alignment
    pub _padding: [u8; 2],
}

// === Rect ===

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct RectInstance {
    pub rect: LayoutRect,
    pub color: u32,
}

/// Rounded rectangle instance with border support (for SDF rendering)
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct RoundedRectInstance {
    pub rect: LayoutRect,
    pub color: u32,
    pub border_color: u32,
    pub corner_radius: f32,
    pub border_width: f32,
}

// === Text Data ===

/// Text range in bytes
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Pod, Zeroable)]
pub struct ByteRange {
    pub start: usize,
    pub end: usize,
}

/// RGBA color
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default, Pod, Zeroable)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Color {
    pub const WHITE: Color = Color {
        r: 1.0,
        g: 1.0,
        b: 1.0,
        a: 1.0,
    };
    pub const BLACK: Color = Color {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 1.0,
    };
    pub const TRANSPARENT: Color = Color {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 0.0,
    };

    /// Convert to u32 for GPU
    pub fn to_u32(&self) -> u32 {
        let r = (self.r * 255.0) as u32;
        let g = (self.g * 255.0) as u32;
        let b = (self.b * 255.0) as u32;
        let a = (self.a * 255.0) as u32;
        (a << 24) | (b << 16) | (g << 8) | r
    }
}

// === Widget Viewport ===

/// Widget-specific viewport for independent positioning and scrolling
#[derive(Debug, Clone)]
pub struct WidgetViewport {
    /// Widget's bounds in window space (after global margin)
    pub bounds: LayoutRect,

    /// Widget's own scroll position
    pub scroll: LayoutPos,

    /// Widget's content margin (for line numbers, gutter, etc)
    pub content_margin: LayoutPos,

    /// Widget ID for plugin association
    pub widget_id: u64,
}

impl WidgetViewport {
    pub fn new(bounds: LayoutRect) -> Self {
        Self {
            bounds,
            scroll: LayoutPos::new(0.0, 0.0),
            content_margin: LayoutPos::new(0.0, 0.0),
            widget_id: 0,
        }
    }

    /// Transform widget-local position to window position
    pub fn local_to_window(&self, pos: LayoutPos) -> LayoutPos {
        LayoutPos::new(
            self.bounds.x.0 + self.content_margin.x.0 + pos.x.0 - self.scroll.x.0,
            self.bounds.y.0 + self.content_margin.y.0 + pos.y.0 - self.scroll.y.0,
        )
    }

    /// Transform window position to widget-local position
    pub fn window_to_local(&self, pos: LayoutPos) -> LayoutPos {
        LayoutPos::new(
            pos.x.0 - self.bounds.x.0 - self.content_margin.x.0 + self.scroll.x.0,
            pos.y.0 - self.bounds.y.0 - self.content_margin.y.0 + self.scroll.y.0,
        )
    }

    /// Check if a widget-local position is visible
    pub fn is_visible(&self, pos: LayoutPos) -> bool {
        let window_pos = self.local_to_window(pos);
        self.bounds.contains(window_pos)
    }
}

// === Viewport Info for Plugins ===

/// Simplified viewport information for plugins
/// This provides the essential coordinate transformation data
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct ViewportInfo {
    /// Current scroll position in layout space
    pub scroll: LayoutPos,
    /// Logical size (DPI-independent)
    pub logical_size: LogicalSize,
    /// Physical size (device pixels)
    pub physical_size: PhysicalSize,
    /// HiDPI scale factor
    pub scale_factor: f32,
    /// Line height in logical pixels
    pub line_height: f32,
    /// Font size in logical pixels
    pub font_size: f32,
    /// Document margin (left, top) - DEPRECATED: use widget viewports
    pub margin: LayoutPos,
    /// Global margin for UI chrome (tabs, toolbar, etc.)
    pub global_margin: LayoutPos,
}

impl ViewportInfo {
    /// Transform layout position to view position (apply scroll)
    pub fn layout_to_view(&self, pos: LayoutPos) -> ViewPos {
        ViewPos::new(pos.x.0 - self.scroll.x.0, pos.y.0 - self.scroll.y.0)
    }

    /// Transform layout position to physical position
    pub fn layout_to_physical(&self, pos: LayoutPos) -> PhysicalPos {
        let view = self.layout_to_view(pos);
        PhysicalPos {
            x: PhysicalPixels(view.x.0 * self.scale_factor),
            y: PhysicalPixels(view.y.0 * self.scale_factor),
        }
    }

    /// Transform layout rectangle to view rectangle
    pub fn layout_rect_to_view(&self, rect: LayoutRect) -> ViewRect {
        ViewRect::new(
            rect.x.0 - self.scroll.x.0,
            rect.y.0 - self.scroll.y.0,
            rect.width.0,
            rect.height.0,
        )
    }

    /// Get font size from line height (approximate)
    pub fn font_size(&self) -> f32 {
        self.line_height / 1.4 // Standard line height multiplier
    }
}
