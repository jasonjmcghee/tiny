//! Unified widget trait for UI components with common rendering needs

use crate::coordinates::Viewport;
use crate::scroll::Scrollable;
use std::sync::Arc;
use tiny_core::tree::Rect;
use tiny_font::SharedFontSystem;
use tiny_sdk::types::{GlyphInstance, RectInstance, RoundedRectInstance};

/// Unified widget trait combining scrolling, bounds, and rendering
pub trait Widget: Scrollable {
    /// Calculate widget bounds based on viewport
    fn calculate_bounds(&mut self, viewport: &Viewport);

    /// Get current bounds
    fn get_bounds(&self) -> Rect;

    /// Collect glyphs for batched rendering with per-view scissor rects
    fn collect_glyphs(&mut self, font_system: &Arc<SharedFontSystem>) -> Vec<(Vec<GlyphInstance>, (u32, u32, u32, u32))>;

    /// Collect background rectangles (selection highlights, backgrounds, etc.)
    fn collect_background_rects(&self) -> Vec<RectInstance>;

    /// Get optional rounded rect frame for overlays
    fn get_frame_rounded_rect(&self) -> Option<RoundedRectInstance> {
        None
    }

    /// Check if widget is visible
    fn is_visible(&self) -> bool;
}

/// Macro to implement Widget trait + inherent methods for types that delegate to an inner field
#[macro_export]
macro_rules! impl_widget_delegate {
    ($type:ty, $field:ident) => {
        // Trait implementation
        impl tiny_ui::Widget for $type {
            fn calculate_bounds(&mut self, viewport: &tiny_ui::Viewport) {
                self.$field.calculate_bounds(viewport);
            }

            fn get_bounds(&self) -> tiny_core::tree::Rect {
                self.$field.get_bounds()
            }

            fn collect_glyphs(&mut self, font_system: &std::sync::Arc<tiny_font::SharedFontSystem>)
                -> Vec<(Vec<tiny_sdk::GlyphInstance>, (u32, u32, u32, u32))> {
                self.$field.collect_glyphs(font_system)
            }

            fn collect_background_rects(&self) -> Vec<tiny_sdk::types::RectInstance> {
                self.$field.collect_background_rects()
            }

            fn get_frame_rounded_rect(&self) -> Option<tiny_sdk::types::RoundedRectInstance> {
                self.$field.get_frame_rounded_rect()
            }

            fn is_visible(&self) -> bool {
                self.visible
            }
        }

        // Inherent methods for convenience (so callers don't need to import Widget trait)
        impl $type {
            pub fn calculate_bounds(&mut self, viewport: &tiny_ui::Viewport) {
                <Self as tiny_ui::Widget>::calculate_bounds(self, viewport)
            }

            pub fn get_bounds(&self) -> tiny_core::tree::Rect {
                <Self as tiny_ui::Widget>::get_bounds(self)
            }

            pub fn collect_glyphs(&mut self, font_system: &std::sync::Arc<tiny_font::SharedFontSystem>)
                -> Vec<(Vec<tiny_sdk::GlyphInstance>, (u32, u32, u32, u32))> {
                <Self as tiny_ui::Widget>::collect_glyphs(self, font_system)
            }

            pub fn collect_background_rects(&self) -> Vec<tiny_sdk::types::RectInstance> {
                <Self as tiny_ui::Widget>::collect_background_rects(self)
            }

            pub fn get_frame_rounded_rect(&self) -> Option<tiny_sdk::types::RoundedRectInstance> {
                <Self as tiny_ui::Widget>::get_frame_rounded_rect(self)
            }
        }
    };
}
