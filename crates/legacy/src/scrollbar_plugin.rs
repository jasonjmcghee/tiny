//! Scrollbar plugin - renders scrollbars for scrollable TextViews

use tiny_core::tree::Rect;
use tiny_sdk::types::{Color, LayoutRect, RoundedRectInstance};
use tiny_ui::{coordinates::Viewport, text_view::TextView};

/// Default scrollbar area width in logical pixels
const SCROLLBAR_AREA_WIDTH: f32 = 16.0;
/// Scrollbar margin from right edge (logical pixels)
const SCROLLBAR_MARGIN_RIGHT: f32 = 4.0;
/// Track padding left/right (logical pixels)
const TRACK_PADDING_X: f32 = 0.0;
/// Track width (area width minus padding on both sides)
const TRACK_WIDTH: f32 = SCROLLBAR_AREA_WIDTH - (TRACK_PADDING_X * 2.0);
/// Minimum thumb height (logical pixels)
const MIN_THUMB_HEIGHT: f32 = 16.0;
/// Scrollbar corner radius (logical pixels)
const CORNER_RADIUS: f32 = 2.0;

/// Plugin that renders scrollbars for scrollable content
pub struct ScrollbarPlugin {
    /// Whether scrollbar is visible (controlled by hover state)
    pub visible: bool,
    /// Scrollbar track color (background)
    track_color: Color,
    /// Scrollbar thumb color (foreground)
    thumb_color: Color,
    /// Last mouse position (for hover detection)
    last_mouse_pos: Option<(f32, f32)>,
    /// Whether the thumb is currently being dragged
    pub is_dragging: bool,
    /// Initial mouse Y position when drag started
    drag_start_y: f32,
    /// Initial scroll position when drag started
    pub drag_start_scroll: f32,
    /// Cached content height (updated when collecting rects)
    pub cached_content_height: f32,
    /// Last scroll activity time (for keeping visible during scroll)
    last_scroll_time: std::time::Instant,
}

impl ScrollbarPlugin {
    pub fn new() -> Self {
        Self {
            visible: false,
            track_color: Color {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 0.0, // Transparent track (only show thumb)
            },
            thumb_color: Color {
                r: 1.0,
                g: 1.0,
                b: 1.0,
                a: 0.2, // 20% opacity white
            },
            last_mouse_pos: None,
            is_dragging: false,
            drag_start_y: 0.0,
            drag_start_scroll: 0.0,
            cached_content_height: 0.0,
            last_scroll_time: std::time::Instant::now() - std::time::Duration::from_secs(10),
        }
    }

    /// Update mouse position for hover detection
    /// Returns true if visibility changed (caller should request redraw)
    pub fn update_mouse_position(
        &mut self,
        x: f32,
        y: f32,
        viewport: &Viewport,
        bounds: &Rect,
    ) -> bool {
        self.last_mouse_pos = Some((x, y));

        let old_visible = self.visible;

        // Keep visible while dragging or recently scrolled
        let recently_scrolled = self.last_scroll_time.elapsed() < std::time::Duration::from_millis(1000);

        if self.is_dragging || recently_scrolled {
            // Keep visible, ensure it's set
            if !self.visible {
                self.visible = true;
                return true; // Visibility changed
            }
            return false; // Already visible, no change
        }

        // Check if mouse is in the scrollbar area (right edge of bounds)
        let scrollbar_x =
            bounds.x.0 + bounds.width.0 - SCROLLBAR_AREA_WIDTH - SCROLLBAR_MARGIN_RIGHT;
        let scrollbar_right = bounds.x.0 + bounds.width.0;

        self.visible = x >= scrollbar_x
            && x <= scrollbar_right
            && y >= bounds.y.0
            && y <= bounds.y.0 + bounds.height.0;


        // Return true if visibility changed
        old_visible != self.visible
    }

    /// Force visibility on/off (useful for debugging or always-visible mode)
    pub fn set_visible(&mut self, visible: bool) {
        self.visible = visible;
    }

    /// Calculate scrollbar thumb position and size
    fn calculate_thumb_rect(
        &self,
        viewport: &Viewport,
        bounds: &Rect,
        content_height: f32,
    ) -> Option<LayoutRect> {
        if !self.visible {
            return None;
        }

        // Don't show scrollbar if content fits in viewport
        if content_height <= bounds.height.0 {
            return None;
        }

        // Calculate scrollbar track dimensions (full height, centered in area with X padding)
        let area_x = bounds.x.0 + bounds.width.0 - SCROLLBAR_AREA_WIDTH - SCROLLBAR_MARGIN_RIGHT;
        let track_x = area_x + TRACK_PADDING_X;
        let track_y = bounds.y.0;
        let track_height = bounds.height.0;

        // Calculate thumb height based on viewport/content ratio
        let viewport_ratio = bounds.height.0 / content_height;
        let thumb_height = (track_height * viewport_ratio).max(MIN_THUMB_HEIGHT);

        // Calculate thumb position based on scroll position
        let max_scroll = (content_height - bounds.height.0).max(0.0);
        let scroll_ratio = if max_scroll > 0.0 {
            viewport.scroll.y.0 / max_scroll
        } else {
            0.0
        };

        // Available space for thumb to move
        let available_space = track_height - thumb_height;
        let thumb_y = track_y + (available_space * scroll_ratio);

        Some(LayoutRect::new(track_x, thumb_y, TRACK_WIDTH, thumb_height))
    }

    /// Check if we need a redraw soon (for timeout-based hide)
    pub fn needs_redraw_soon(&self) -> bool {
        if !self.is_dragging && self.visible {
            let elapsed = self.last_scroll_time.elapsed();
            // Request redraw if we're within the timeout window
            elapsed < std::time::Duration::from_millis(1100)
        } else {
            false
        }
    }

    /// Collect rounded rectangles for rendering
    pub fn collect_rounded_rects(
        &mut self,
        viewport: &Viewport,
        bounds: &Rect,
        content_height: f32,
    ) -> Vec<RoundedRectInstance> {
        // Cache content height for use in click handlers
        self.cached_content_height = content_height;

        let mut rects = Vec::new();

        // Don't render anything if content isn't scrollable
        if content_height <= bounds.height.0 {
            return rects;
        }

        // Update visibility based on scroll activity and dragging
        // NOTE: Hover detection is handled in update_mouse_position
        let recently_scrolled = self.last_scroll_time.elapsed() < std::time::Duration::from_millis(1000);

        // Show if dragging or recently scrolled
        if self.is_dragging || recently_scrolled {
            self.visible = true;
        }
        // Only hide if timeout expired, not dragging, AND mouse not hovering
        else if self.visible && !recently_scrolled && !self.is_dragging {
            // Check if mouse is currently in scrollbar area
            if let Some((x, y)) = self.last_mouse_pos {
                let scrollbar_x = bounds.x.0 + bounds.width.0 - SCROLLBAR_AREA_WIDTH - SCROLLBAR_MARGIN_RIGHT;
                let scrollbar_right = bounds.x.0 + bounds.width.0;
                let hovering = x >= scrollbar_x && x <= scrollbar_right
                    && y >= bounds.y.0 && y <= bounds.y.0 + bounds.height.0;

                // Only hide if not hovering
                if !hovering {
                    self.visible = false;
                }
            } else {
                // No mouse position tracked, safe to hide
                self.visible = false;
            }
        }

        // Only render track and thumb if visible (hovering or scrolling)
        if !self.visible {
            return rects;
        }

        // Calculate track dimensions (full height, centered with X padding)
        let area_x = bounds.x.0 + bounds.width.0 - SCROLLBAR_AREA_WIDTH - SCROLLBAR_MARGIN_RIGHT;
        let track_x = area_x + TRACK_PADDING_X;
        let track_y = bounds.y.0;
        let track_height = bounds.height.0;

        // Render track (no rounded edges, 4px padding on left/right)
        let track_color_rgba = 0xFFFFFF08; // White at 5% (13/255 ≈ 0.05)
        rects.push(RoundedRectInstance {
            rect: LayoutRect::new(track_x, track_y, TRACK_WIDTH, track_height),
            color: track_color_rgba,
            border_color: 0x00000000,
            corner_radius: 0.0, // No rounding for track
            border_width: 0.0,
        });

        // Render thumb
        if let Some(thumb_rect) = self.calculate_thumb_rect(viewport, bounds, content_height) {
            // White at 20% opacity in RGBA format
            let thumb_color_rgba = 0xFFFFFF16; // White at 20% (51/255 = 0.2)

            rects.push(RoundedRectInstance {
                rect: thumb_rect,
                color: thumb_color_rgba,
                border_color: 0x00000000,
                corner_radius: CORNER_RADIUS,
                border_width: 0.0,
            });
        }

        rects
    }

    /// Check if point is over the scrollbar area (track or thumb)
    /// Uses cached content_height from last render
    pub fn is_point_in_scrollbar_area(&self, x: f32, y: f32, bounds: &Rect) -> bool {
        // Use cached content height
        let content_height = self.cached_content_height;

        // Don't handle clicks if content isn't scrollable
        if content_height <= bounds.height.0 {
            return false;
        }

        let scrollbar_x =
            bounds.x.0 + bounds.width.0 - SCROLLBAR_AREA_WIDTH - SCROLLBAR_MARGIN_RIGHT;
        let scrollbar_right = bounds.x.0 + bounds.width.0;

        x >= scrollbar_x
            && x <= scrollbar_right
            && y >= bounds.y.0
            && y <= bounds.y.0 + bounds.height.0
    }

    /// Check if point is over the scrollbar thumb (for drag detection)
    pub fn is_point_over_thumb(
        &self,
        x: f32,
        y: f32,
        viewport: &Viewport,
        bounds: &Rect,
        content_height: f32,
    ) -> bool {
        // Must be visible and in scrollbar area first
        if !self.visible {
            return false;
        }

        if let Some(thumb_rect) = self.calculate_thumb_rect(viewport, bounds, content_height) {
            x >= thumb_rect.x.0
                && x <= thumb_rect.x.0 + thumb_rect.width.0
                && y >= thumb_rect.y.0
                && y <= thumb_rect.y.0 + thumb_rect.height.0
        } else {
            false
        }
    }

    /// Start dragging the scrollbar thumb
    pub fn start_drag(&mut self, mouse_y: f32, viewport: &Viewport) {
        self.is_dragging = true;
        self.visible = true; // Ensure visible while dragging
        self.drag_start_y = mouse_y;
        self.drag_start_scroll = viewport.scroll.y.0;
    }

    /// Stop dragging the scrollbar thumb
    pub fn stop_drag(&mut self) {
        self.is_dragging = false;
        // Visibility will be updated on next mouse move
    }

    /// Update scroll time (call whenever scroll happens)
    pub fn mark_scroll_activity(&mut self) {
        self.last_scroll_time = std::time::Instant::now();
    }

    /// Handle scrollbar drag (returns new scroll position if dragged)
    /// Uses cached content_height from last render
    pub fn handle_drag(
        &mut self,
        mouse_y: f32,
        viewport: &Viewport,
        bounds: &Rect,
    ) -> Option<f32> {
        let content_height = self.cached_content_height;

        if !self.is_dragging || content_height <= bounds.height.0 {
            return None;
        }

        let track_height = bounds.height.0;
        let viewport_ratio = bounds.height.0 / content_height;
        let thumb_height = (track_height * viewport_ratio).max(MIN_THUMB_HEIGHT);
        let available_space = track_height - thumb_height;

        // Calculate drag delta
        let drag_delta_y = mouse_y - self.drag_start_y;

        // Convert drag delta to scroll delta
        let max_scroll = (content_height - bounds.height.0).max(0.0);
        let scroll_delta = if available_space > 0.0 {
            (drag_delta_y / available_space) * max_scroll
        } else {
            0.0
        };

        let new_scroll = (self.drag_start_scroll + scroll_delta)
            .max(0.0)
            .min(max_scroll);

        Some(new_scroll)
    }

    /// Handle click on scrollbar track (jump to position)
    /// Centers the thumb at the click position
    pub fn handle_track_click(
        &self,
        click_y: f32,
        viewport: &Viewport,
        bounds: &Rect,
        content_height: f32,
    ) -> Option<f32> {
        if !self.visible || content_height <= bounds.height.0 {
            return None;
        }

        let track_y = bounds.y.0;
        let track_height = bounds.height.0;

        // Calculate thumb dimensions
        let viewport_ratio = bounds.height.0 / content_height;
        let thumb_height = (track_height * viewport_ratio).max(MIN_THUMB_HEIGHT);
        let available_space = track_height - thumb_height;

        // Center thumb at click position
        // Click position is where we want the CENTER of the thumb to be
        let thumb_center_offset = click_y - track_y;
        let thumb_top_offset = thumb_center_offset - (thumb_height / 2.0);

        // Clamp to available space
        let clamped_offset = thumb_top_offset.clamp(0.0, available_space);

        // Convert thumb position to scroll position
        let max_scroll = (content_height - bounds.height.0).max(0.0);
        let scroll_ratio = if available_space > 0.0 {
            clamped_offset / available_space
        } else {
            0.0
        };

        let new_scroll = scroll_ratio * max_scroll;

        Some(new_scroll)
    }
}

impl Default for ScrollbarPlugin {
    fn default() -> Self {
        Self::new()
    }
}
