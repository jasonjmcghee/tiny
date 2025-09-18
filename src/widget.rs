//! Widget system where everything visual is a widget, including text
//!
//! Text rendering uses the consolidated FontSystem from font.rs

use crate::coordinates::{LogicalPixels, LogicalSize};
use crate::tree::{Point, Widget};
use std::sync::Arc;

// === Content Type for TextWidget ===

/// Describes what type of content a TextWidget contains
#[derive(Debug, Clone)]
pub enum ContentType {
    /// Full lines (legacy, for non-virtualized content)
    Full,
    /// Extracted columns with horizontal offset (NoWrap mode)
    Columns {
        /// Starting column in the original line
        start_col: usize,
        /// X offset for rendering (negative for scrolled content)
        x_offset: f32
    },
    /// Wrapped visual lines (SoftWrap mode)
    Wrapped {
        /// The visual lines after wrapping
        visual_lines: Vec<String>
    },
}

impl Default for ContentType {
    fn default() -> Self {
        ContentType::Full
    }
}

// === Core Widget Types ===
/// Text widget - renders text using the consolidated FontSystem
pub struct TextWidget {
    /// UTF-8 text content
    pub text: Arc<[u8]>,
    /// Style ID for font/size/color
    pub style: StyleId,
    /// Pre-calculated size (measured with actual font system)
    pub size: LogicalSize,
    /// Type of content (full, columns, or wrapped)
    pub content_type: ContentType,
}

impl Clone for TextWidget {
    fn clone(&self) -> Self {
        Self {
            text: Arc::clone(&self.text),
            style: self.style,
            size: self.size,
            content_type: self.content_type.clone(),
        }
    }
}

/// Cursor widget - blinking text cursor
#[derive(Clone)]
pub struct CursorWidget {
    /// Style for cursor (color, width)
    pub style: CursorStyle,
    /// Animation state
    pub blink_phase: f32,
}

/// Selection widget - highlight for selected text
#[derive(Clone)]
pub struct SelectionWidget {
    /// Byte range of selection
    pub range: std::ops::Range<usize>,
    /// Selection color
    pub color: u32,
}

/// Line number widget
#[derive(Clone)]
pub struct LineNumberWidget {
    pub line: u32,
    pub style: StyleId,
}

/// Diagnostic widget - error/warning underline
#[derive(Clone)]
pub struct DiagnosticWidget {
    pub severity: Severity,
    pub message: Arc<str>,
    pub range: std::ops::Range<usize>,
}

/// Style widget - changes text appearance
#[derive(Clone)]
pub struct StyleWidget {
    /// Where style ends
    pub end_byte: usize,
    /// New style to apply
    pub style: StyleId,
}

// === Supporting Types ===

pub type StyleId = u32;

#[derive(Clone)]
pub struct CursorStyle {
    pub color: u32,
    pub width: f32,
}

#[derive(Clone, Copy)]
pub enum Severity {
    Error,
    Warning,
    Info,
    Hint,
}

// === Widget Implementations ===

impl Widget for TextWidget {
    fn measure(&self) -> LogicalSize {
        // Return the pre-calculated size that was measured using the actual font system
        self.size
    }

    fn z_index(&self) -> i32 {
        0 // Text is base layer
    }

    fn hit_test(&self, pt: Point) -> bool {
        let size = self.measure();
        pt.x >= LogicalPixels(0.0) && pt.x <= size.width && pt.y >= LogicalPixels(0.0) && pt.y <= size.height
    }

    fn paint(&self, ctx: &mut crate::tree::PaintContext<'_>) {
        use crate::render::{GlyphInstance, RenderOp};

        let text = std::str::from_utf8(&self.text).unwrap_or("");
        if text.is_empty() {
            return;
        }

        // Get the shared font system from context
        let font_system = match ctx.font_system {
            Some(fs) => fs,
            None => {
                println!("WARNING: No font system in paint context");
                return;
            }
        };

        // Use font size from viewport metrics
        let font_size = ctx.viewport.metrics.font_size;
        let scale_factor = ctx.viewport.scale_factor;
        let line_height = ctx.viewport.metrics.line_height;

        // TextWidget now handles multi-line text for the virtualized visible range
        let lines: Vec<&str> = text.lines().collect();

        // Stats for debugging culling effectiveness
        let total_chars = text.chars().count();
        let mut rendered_chars = 0usize;

        let mut all_glyph_instances = Vec::new();
        let mut y_offset = 0.0;
        let mut global_byte_pos = 0;

        // Handle different content types
        let x_base_offset = match &self.content_type {
            ContentType::Columns { x_offset, start_col } => {
                if *x_offset != 0.0 {
                    println!("TextWidget applying x_offset={:.1} for columns starting at {}", x_offset, start_col);
                }
                *x_offset
            }
            _ => 0.0,
        };

        for line_text in lines.iter() {
            // Layout this single line
            let layout = font_system.layout_text_scaled(line_text, font_size, scale_factor);
            rendered_chars += line_text.chars().count();

            let mut byte_pos = 0;
            for glyph in &layout.glyphs {
                let mut color = glyph.color;

                // Apply debug colorizing for off-screen content
                if ctx.debug_offscreen {
                    color = 0xFF0000FF; // Bright red for off-screen content
                } else if let Some(text_styles) = ctx.text_styles {
                    let char_bytes = glyph.char.len_utf8();
                    let effects = text_styles.get_effects_in_range(
                        global_byte_pos + byte_pos..global_byte_pos + byte_pos + char_bytes
                    );

                    for effect in effects {
                        if let crate::text_effects::EffectType::Color(new_color) = effect.effect {
                            color = new_color;
                        }
                    }
                    byte_pos += char_bytes;
                }

                // Glyphs from font system are in physical pixels relative to (0,0)
                // Convert to logical and add layout position plus line offset
                let glyph_logical_x = glyph.pos.x.0 / ctx.viewport.scale_factor;
                let glyph_logical_y = glyph.pos.y.0 / ctx.viewport.scale_factor;

                // Debug first glyph position calculation
                if all_glyph_instances.is_empty() && x_base_offset != 0.0 {
                    println!("  First glyph positioning: layout.x={}, x_offset={}, glyph_x={} -> final={}",
                             ctx.layout_pos.x.0, x_base_offset, glyph_logical_x,
                             ctx.layout_pos.x.0 + x_base_offset + glyph_logical_x);
                }

                let glyph_pos = crate::coordinates::LayoutPos::new(
                    ctx.layout_pos.x.0 + x_base_offset + glyph_logical_x,
                    ctx.layout_pos.y.0 + y_offset + glyph_logical_y,
                );

                all_glyph_instances.push(GlyphInstance {
                    glyph_id: 0, // Not used anymore
                    pos: glyph_pos,  // Now in layout space (logical pixels)
                    color,
                    tex_coords: glyph.tex_coords,
                });
            }

            // Update position for next line
            global_byte_pos += line_text.len() + 1; // +1 for newline
            y_offset += line_height;
        }

        // Debug stats for culling effectiveness
        let glyph_count = all_glyph_instances.len();
        println!("GLYPH STATS: Laid out {} glyphs for {} chars (visible: {}, total: {}, culled: {})",
                 glyph_count, rendered_chars, rendered_chars, total_chars,
                 total_chars.saturating_sub(rendered_chars));

        // Emit render command in LAYOUT space
        if !all_glyph_instances.is_empty() {
            ctx.commands.push(RenderOp::Glyphs {
                glyphs: Arc::from(all_glyph_instances.into_boxed_slice()),
                style: self.style,
            });
        }
    }

    fn clone_box(&self) -> Arc<dyn Widget> {
        Arc::new(self.clone())
    }
}

impl Widget for CursorWidget {
    fn measure(&self) -> LogicalSize {
        LogicalSize::new(self.style.width, 19.6) // Use standard line height (14pt * 1.4)
    }

    fn z_index(&self) -> i32 {
        100 // Cursor on top
    }

    fn hit_test(&self, _pt: Point) -> bool {
        false // Cursor doesn't capture clicks
    }

    fn paint(&self, ctx: &mut crate::tree::PaintContext<'_>) {
        use crate::render::RenderOp;
        use crate::tree::Rect;

        // Apply blinking animation
        let alpha = ((self.blink_phase * 2.0).sin() * 0.5 + 0.5).max(0.3);
        let color = (self.style.color & 0x00FFFFFF) | (((alpha * 255.0) as u32) << 24);

        // Use line height from viewport metrics
        let line_height = ctx.viewport.metrics.line_height;

        ctx.commands.push(RenderOp::Rect {
            rect: Rect {
                x: ctx.layout_pos.x,
                y: ctx.layout_pos.y,
                width: LogicalPixels(self.style.width),
                height: LogicalPixels(line_height),
            },
            color,
        });
    }

    fn clone_box(&self) -> Arc<dyn Widget> {
        Arc::new(self.clone())
    }
}

impl Widget for SelectionWidget {
    fn measure(&self) -> LogicalSize {
        // Size determined by text range
        LogicalSize::new(0.0, 0.0)
    }

    fn z_index(&self) -> i32 {
        -1 // Selection behind text
    }

    fn hit_test(&self, _pt: Point) -> bool {
        false
    }

    fn paint(&self, ctx: &mut crate::tree::PaintContext<'_>) {
        use crate::render::RenderOp;
        use crate::tree::Rect;

        // TODO: Calculate actual bounds based on text range
        // For now, draw a simple rectangle
        let width = LogicalPixels(100.0); // Would be calculated from text metrics
        let height = LogicalPixels(ctx.viewport.metrics.line_height);

        ctx.commands.push(RenderOp::Rect {
            rect: Rect {
                x: ctx.layout_pos.x,
                y: ctx.layout_pos.y,
                width,
                height,
            },
            color: self.color,
        });
    }

    fn clone_box(&self) -> Arc<dyn Widget> {
        Arc::new(self.clone())
    }
}

impl Widget for LineNumberWidget {
    fn measure(&self) -> LogicalSize {
        // Measure line number text
        let text = format!("{}", self.line);
        // Approximate size for line numbers - will be properly measured during paint
        let width = text.len() as f32 * 8.4;
        let height = 19.6;
        LogicalSize::new(width, height)
    }

    fn z_index(&self) -> i32 {
        0
    }

    fn hit_test(&self, pt: Point) -> bool {
        let size = self.measure();
        pt.x >= LogicalPixels(0.0) && pt.x <= size.width && pt.y >= LogicalPixels(0.0) && pt.y <= size.height
    }

    fn paint(&self, ctx: &mut crate::tree::PaintContext<'_>) {
        // Create text widget for the line number and paint it
        let text = format!("{}", self.line);
        // For line numbers, we can use approximate size since they're simple
        let width = text.len() as f32 * 8.4;
        let height = 19.6;
        let widget = TextWidget {
            text: Arc::from(text.as_bytes()),
            style: self.style,
            size: LogicalSize::new(width, height),
            content_type: ContentType::default(),
        };
        widget.paint(ctx);
    }

    fn clone_box(&self) -> Arc<dyn Widget> {
        Arc::new(self.clone())
    }
}

impl Widget for DiagnosticWidget {
    fn measure(&self) -> LogicalSize {
        LogicalSize::new(0.0, 2.0) // Underline height
    }

    fn z_index(&self) -> i32 {
        10 // Above text
    }

    fn hit_test(&self, _pt: Point) -> bool {
        false
    }

    fn paint(&self, ctx: &mut crate::tree::PaintContext<'_>) {
        use crate::render::RenderOp;
        use crate::tree::Point;

        let color = match self.severity {
            Severity::Error => 0xFFFF0000,   // Red
            Severity::Warning => 0xFFFF8800, // Orange
            Severity::Info => 0xFF0088FF,    // Blue
            Severity::Hint => 0xFF888888,    // Gray
        };

        // Draw wavy underline
        let width = 100.0; // TODO: Calculate from text range
        let segments = (width / 4.0) as i32;
        let base_y = ctx.layout_pos.y + ctx.viewport.metrics.line_height - 2.0; // Position at bottom of line

        for i in 0..segments {
            let x1 = ctx.layout_pos.x + (i as f32) * 4.0;
            let x2 = ctx.layout_pos.x + ((i + 1) as f32) * 4.0;
            let y_offset = if i % 2 == 0 { 0.0 } else { 1.0 };

            ctx.commands.push(RenderOp::Line {
                from: Point {
                    x: x1,
                    y: base_y + y_offset,
                },
                to: Point {
                    x: x2,
                    y: base_y + 1.0 - y_offset,
                },
                color,
                width: 1.0,
            });
        }
    }

    fn clone_box(&self) -> Arc<dyn Widget> {
        Arc::new(self.clone())
    }
}

impl Widget for StyleWidget {
    fn measure(&self) -> LogicalSize {
        LogicalSize::new(0.0, 0.0) // Style has no size
    }

    fn z_index(&self) -> i32 {
        0
    }

    fn hit_test(&self, _pt: Point) -> bool {
        false
    }

    fn paint(&self, _ctx: &mut crate::tree::PaintContext) {
        // StyleWidget doesn't render anything - it's just metadata
        // The TextWidget will look for these when rendering text
    }

    fn clone_box(&self) -> Arc<dyn Widget> {
        Arc::new(self.clone())
    }
}

// === Public API ===

/// Create text widget from string
/// Note: This creates a widget with default size - use render_text() for accurate sizing
pub fn text(s: &str) -> Arc<dyn Widget> {
    Arc::new(TextWidget {
        text: Arc::from(s.as_bytes()),
        style: 0, // Default style
        size: LogicalSize::new(0.0, 0.0), // Will be calculated properly in render_text
        content_type: ContentType::default(),
    })
}

/// Create cursor widget
pub fn cursor() -> Arc<dyn Widget> {
    Arc::new(CursorWidget {
        style: CursorStyle {
            color: 0xFFFFFFFF,
            width: 2.0,
        },
        blink_phase: 0.0,
    })
}

/// Create selection widget
pub fn selection(range: std::ops::Range<usize>) -> Arc<dyn Widget> {
    Arc::new(SelectionWidget {
        range,
        color: 0x4080FF80, // Semi-transparent blue
    })
}

/// Create line number widget
pub fn line_number(line: u32) -> Arc<dyn Widget> {
    Arc::new(LineNumberWidget { line, style: 0 })
}

/// Create diagnostic widget
pub fn diagnostic(
    severity: Severity,
    message: &str,
    range: std::ops::Range<usize>,
) -> Arc<dyn Widget> {
    Arc::new(DiagnosticWidget {
        severity,
        message: Arc::from(message),
        range,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coordinates::{LayoutPos, LogicalPixels, PhysicalPixels, Viewport};
    use crate::font::SharedFontSystem;
    use crate::render::RenderOp;
    use crate::tree::PaintContext;
    use std::sync::Arc;

    #[test]
    fn test_text_widget_paint() {
        let widget = text("X");
        let font_system = Arc::new(SharedFontSystem::new());
        let mut viewport = Viewport::new(800.0, 600.0, 1.0);
        viewport.set_font_system(font_system.clone());

        let mut commands = Vec::new();
        let mut ctx = PaintContext {
            layout_pos: LayoutPos { x: LogicalPixels(10.0), y: LogicalPixels(20.0) },
            view_pos: None,
            doc_pos: None,
            commands: &mut commands,
            text_styles: None,
            font_system: Some(&font_system),
            viewport: &viewport,
            debug_offscreen: false,
        };

        widget.paint(&mut ctx);

        assert_eq!(commands.len(), 1);
        match &commands[0] {
            RenderOp::Glyphs { glyphs, .. } => {
                assert_eq!(glyphs.len(), 1);
                let glyph = &glyphs[0];
                assert_eq!(glyph.pos.x, LogicalPixels(10.0));
                assert_eq!(glyph.pos.y, LogicalPixels(24.0)); // 20.0 + 4.0 baseline offset
                assert_eq!(glyph.color, 0xFFFFFFFF);
            }
            _ => panic!("Expected Glyphs command"),
        }
    }

    #[test]
    fn test_cursor_widget_render() {
        let cursor = CursorWidget {
            style: CursorStyle {
                color: 0xFF0000FF, // Red
                width: 2.0,
            },
            blink_phase: 0.0,
        };

        let viewport = Viewport::new(800.0, 600.0, 1.0);
        let mut commands = Vec::new();
        let mut ctx = PaintContext {
            layout_pos: LayoutPos { x: LogicalPixels(50.0), y: LogicalPixels(100.0) },
            view_pos: None,
            doc_pos: None,
            commands: &mut commands,
            text_styles: None,
            font_system: None,
            viewport: &viewport,
            debug_offscreen: false,
        };

        cursor.paint(&mut ctx);

        assert_eq!(commands.len(), 1);
        match &commands[0] {
            RenderOp::Rect { rect, color } => {
                assert_eq!(rect.x, LogicalPixels(50.0));
                assert_eq!(rect.y, LogicalPixels(100.0));
                assert_eq!(rect.width, LogicalPixels(2.0));
                assert_eq!(rect.height, LogicalPixels(19.6)); // line_height (14.0 * 1.4)
                assert_eq!(*color, 0x7FFF0000); // 50% opacity red
            }
            _ => panic!("Expected Rect command"),
        }
    }
}
