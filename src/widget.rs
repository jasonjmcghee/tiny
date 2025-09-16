//! Widget system where everything visual is a widget, including text
//!
//! Text rendering uses the consolidated FontSystem from font.rs

use crate::tree::{Point, Widget};
use std::sync::Arc;

// === Core Widget Types ===

/// Text widget - renders text using the consolidated FontSystem
pub struct TextWidget {
    /// UTF-8 text content
    pub text: Arc<[u8]>,
    /// Style ID for font/size/color
    pub style: StyleId,
}

impl Clone for TextWidget {
    fn clone(&self) -> Self {
        Self {
            text: Arc::clone(&self.text),
            style: self.style,
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
    fn measure(&self) -> (f32, f32) {
        // For now, estimate based on text length
        // Real measurement happens during paint when we have font system access
        let text = std::str::from_utf8(&self.text).unwrap_or("");
        let char_count = text.chars().count();
        let estimated_width = char_count as f32 * 8.0; // ~8px per char
        let estimated_height = 20.0; // Single line height
        (estimated_width, estimated_height)
    }

    fn z_index(&self) -> i32 {
        0 // Text is base layer
    }

    fn hit_test(&self, pt: Point) -> bool {
        let (width, height) = self.measure();
        pt.x >= 0.0 && pt.x <= width && pt.y >= 0.0 && pt.y <= height
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

        // Layout text - keep it simple
        let font_size = 13.0;
        println!("    Font layout: font_size={:.1}", font_size);
        let layout = font_system.layout_text_scaled(text, font_size, ctx.viewport.scale_factor);

        // Convert to GPU instances
        let mut glyph_instances = Vec::with_capacity(layout.glyphs.len());
        let base_x = ctx.position.x;
        let base_y = ctx.position.y;

        println!(
            "    TextWidget.paint: base=({:.1}, {:.1}), {} glyphs from font",
            base_x,
            base_y,
            layout.glyphs.len()
        );

        // Track byte position for text effects
        let mut byte_pos = 0;

        for glyph in &layout.glyphs {
            let mut color = glyph.color;

            // Apply text effects if available
            if let Some(text_styles) = ctx.text_styles {
                let char_bytes = glyph.char.len_utf8();
                let effects = text_styles.get_effects_in_range(byte_pos..byte_pos + char_bytes);

                for effect in effects {
                    if let crate::text_effects::EffectType::Color(new_color) = effect.effect {
                        color = new_color;
                        break;
                    }
                }
                byte_pos += char_bytes;
            }

            // Font system works in logical coordinates
            let final_x = base_x + glyph.x;
            let final_y = base_y + glyph.y;

            if byte_pos == 0 {
                // Debug first glyph only
                println!("      First glyph '{}': font_pos=({:.1}, {:.1}) + base=({:.1}, {:.1}) = final=({:.1}, {:.1})",
                         glyph.char, glyph.x, glyph.y, base_x, base_y, final_x, final_y);
            }

            glyph_instances.push(GlyphInstance {
                glyph_id: 0, // Not used anymore
                x: final_x,
                y: final_y,
                color,
                tex_coords: glyph.tex_coords,
            });
        }

        // Emit render command
        if !glyph_instances.is_empty() {
            ctx.commands.push(RenderOp::Glyphs {
                glyphs: Arc::from(glyph_instances.into_boxed_slice()),
                style: self.style,
            });
        }
    }

    fn clone_box(&self) -> Arc<dyn Widget> {
        Arc::new(self.clone())
    }
}

impl Widget for CursorWidget {
    fn measure(&self) -> (f32, f32) {
        (self.style.width, 20.0) // Fixed height cursor
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

        ctx.commands.push(RenderOp::Rect {
            rect: Rect {
                x: ctx.position.x,
                y: ctx.position.y,
                width: self.style.width,
                height: 20.0,
            },
            color,
        });
    }

    fn clone_box(&self) -> Arc<dyn Widget> {
        Arc::new(self.clone())
    }
}

impl Widget for SelectionWidget {
    fn measure(&self) -> (f32, f32) {
        // Size determined by text range
        (0.0, 0.0)
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
        let width = 100.0; // Would be calculated from text metrics
        let height = 20.0;

        ctx.commands.push(RenderOp::Rect {
            rect: Rect {
                x: ctx.position.x,
                y: ctx.position.y,
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
    fn measure(&self) -> (f32, f32) {
        // Measure line number text
        let text = format!("{}", self.line);
        let widget = TextWidget {
            text: Arc::from(text.as_bytes()),
            style: self.style,
        };
        widget.measure()
    }

    fn z_index(&self) -> i32 {
        0
    }

    fn hit_test(&self, pt: Point) -> bool {
        let (width, height) = self.measure();
        pt.x >= 0.0 && pt.x <= width && pt.y >= 0.0 && pt.y <= height
    }

    fn paint(&self, ctx: &mut crate::tree::PaintContext<'_>) {
        // Create text widget for the line number and paint it
        let text = format!("{}", self.line);
        let widget = TextWidget {
            text: Arc::from(text.as_bytes()),
            style: self.style,
        };
        widget.paint(ctx);
    }

    fn clone_box(&self) -> Arc<dyn Widget> {
        Arc::new(self.clone())
    }
}

impl Widget for DiagnosticWidget {
    fn measure(&self) -> (f32, f32) {
        (0.0, 2.0) // Underline height
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

        for i in 0..segments {
            let x1 = ctx.position.x + (i as f32) * 4.0;
            let x2 = ctx.position.x + ((i + 1) as f32) * 4.0;
            let y_offset = if i % 2 == 0 { 0.0 } else { 1.0 };

            ctx.commands.push(RenderOp::Line {
                from: Point {
                    x: x1,
                    y: ctx.position.y + y_offset,
                },
                to: Point {
                    x: x2,
                    y: ctx.position.y + 1.0 - y_offset,
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
    fn measure(&self) -> (f32, f32) {
        (0.0, 0.0) // Style has no size
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
pub fn text(s: &str) -> Arc<dyn Widget> {
    Arc::new(TextWidget {
        text: Arc::from(s.as_bytes()),
        style: 0, // Default style
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
    use crate::coordinates::{LogicalSize, Viewport};
    use crate::font::SharedFontSystem;
    use crate::render::RenderOp;
    use crate::tree::{PaintContext, Point};
    use std::sync::Arc;

    #[test]
    fn test_text_widget_paint() {
        let widget = text("X");
        let font_system = Arc::new(SharedFontSystem::new());
        let mut viewport = Viewport::new(LogicalSize { width: 800.0, height: 600.0 }, 1.0);
        viewport.set_font_system(font_system.clone());

        let mut commands = Vec::new();
        let mut ctx = PaintContext {
            position: Point { x: 10.0, y: 20.0 },
            commands: &mut commands,
            text_styles: None,
            font_system: Some(&font_system),
            viewport: &viewport,
        };

        widget.paint(&mut ctx);

        assert_eq!(commands.len(), 1);
        match &commands[0] {
            RenderOp::Glyphs { glyphs, .. } => {
                assert_eq!(glyphs.len(), 1);
                let glyph = &glyphs[0];
                assert_eq!(glyph.x, 10.0);
                assert_eq!(glyph.y, 24.0); // 20.0 + 4.0 baseline offset
                assert_eq!(glyph.color, 0xFFFFFFFF);
            }
            _ => panic!("Expected Glyphs command"),
        }
    }

    #[test]
    fn test_cursor_widget_render() {
        let cursor = CursorWidget {
            style: CursorStyle {
                color: 0xFFFF0000, // Red
                width: 2.0,
            },
            blink_phase: 0.0,
        };

        let viewport = Viewport::new(LogicalSize { width: 800.0, height: 600.0 }, 1.0);
        let mut commands = Vec::new();
        let mut ctx = PaintContext {
            position: Point { x: 50.0, y: 100.0 },
            commands: &mut commands,
            text_styles: None,
            font_system: None,
            viewport: &viewport,
        };

        cursor.paint(&mut ctx);

        assert_eq!(commands.len(), 1);
        match &commands[0] {
            RenderOp::Rect { rect, color } => {
                assert_eq!(rect.x, 50.0);
                assert_eq!(rect.y, 100.0);
                assert_eq!(rect.width, 2.0);
                assert_eq!(rect.height, 20.0); // line_height
                assert_eq!(*color, 0x7FFF0000); // 50% opacity red
            }
            _ => panic!("Expected Rect command"),
        }
    }
}
