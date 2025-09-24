//! Text Editor API - Provides methods for cursor and selection plugins

use tiny_sdk::{LayoutPos, LayoutRect, LogicalSize, PluginError};
use crate::layout::LayoutCache;

/// API exposed by text editor plugin for other plugins to use
pub struct TextEditorAPI {
    // Cache frequently requested values
    line_height: f32,
    char_width: f32,
}

impl TextEditorAPI {
    pub fn new() -> Self {
        Self {
            line_height: 19.6,  // Default, will be updated from layout
            char_width: 8.4,    // Default, will be updated from layout
        }
    }

    /// Update cached values from layout cache
    pub fn update_from_layout(&mut self, layout: &LayoutCache) {
        self.line_height = layout.line_height();
        self.char_width = layout.char_width();
    }

    /// Handle API calls from other plugins
    pub fn handle_call(
        &mut self,
        method: &str,
        args: &[u8],
        layout: &LayoutCache,
    ) -> Result<Vec<u8>, PluginError> {
        match method {
            "get_char_position" => {
                // Args: byte_offset (usize as 8 bytes)
                if args.len() != 8 {
                    return Err(PluginError::Other("Invalid args for get_char_position".into()));
                }

                let byte_offset = usize::from_le_bytes([
                    args[0], args[1], args[2], args[3],
                    args[4], args[5], args[6], args[7],
                ]);

                let pos = layout.get_char_position(byte_offset)
                    .unwrap_or(LayoutPos::new(0.0, 0.0));

                let mut result = Vec::new();
                result.extend_from_slice(&pos.x.0.to_le_bytes());
                result.extend_from_slice(&pos.y.0.to_le_bytes());
                Ok(result)
            }

            "get_line_height" => {
                // No args, returns f32
                Ok(self.line_height.to_le_bytes().to_vec())
            }

            "get_char_bounds" => {
                // Args: byte_offset (usize as 8 bytes)
                if args.len() != 8 {
                    return Err(PluginError::Other("Invalid args for get_char_bounds".into()));
                }

                let byte_offset = usize::from_le_bytes([
                    args[0], args[1], args[2], args[3],
                    args[4], args[5], args[6], args[7],
                ]);

                let bounds = layout.get_char_bounds(byte_offset)
                    .unwrap_or(LayoutRect::new(0.0, 0.0, self.char_width, self.line_height));

                let mut result = Vec::new();
                result.extend_from_slice(&bounds.x.0.to_le_bytes());
                result.extend_from_slice(&bounds.y.0.to_le_bytes());
                result.extend_from_slice(&bounds.width.0.to_le_bytes());
                result.extend_from_slice(&bounds.height.0.to_le_bytes());
                Ok(result)
            }

            "get_line_bounds" => {
                // Args: line_number (u32 as 4 bytes)
                if args.len() != 4 {
                    return Err(PluginError::Other("Invalid args for get_line_bounds".into()));
                }

                let line_number = u32::from_le_bytes([args[0], args[1], args[2], args[3]]);

                let bounds = layout.get_line_bounds(line_number)
                    .unwrap_or(LayoutRect::new(0.0, 0.0, 800.0, self.line_height));

                let mut result = Vec::new();
                result.extend_from_slice(&bounds.x.0.to_le_bytes());
                result.extend_from_slice(&bounds.y.0.to_le_bytes());
                result.extend_from_slice(&bounds.width.0.to_le_bytes());
                result.extend_from_slice(&bounds.height.0.to_le_bytes());
                Ok(result)
            }

            "get_visible_range" => {
                // Args: viewport y scroll (f32 as 4 bytes), viewport height (f32 as 4 bytes)
                if args.len() != 8 {
                    return Err(PluginError::Other("Invalid args for get_visible_range".into()));
                }

                let scroll_y = f32::from_le_bytes([args[0], args[1], args[2], args[3]]);
                let height = f32::from_le_bytes([args[4], args[5], args[6], args[7]]);

                let (start_line, end_line) = layout.get_visible_lines(scroll_y, height);

                let mut result = Vec::new();
                result.extend_from_slice(&start_line.to_le_bytes());
                result.extend_from_slice(&end_line.to_le_bytes());
                Ok(result)
            }

            "set_viewport_info" => {
                // Args: line_height, width, margin_x, margin_y, scale_factor, scroll_x, scroll_y (7 f32s)
                if args.len() < 28 {
                    return Err(PluginError::Other("Invalid args for set_viewport_info".into()));
                }

                // Just acknowledge receipt - actual viewport is handled by host
                Ok(Vec::new())
            }

            "set_document" => {
                // Args: version (u64) followed by UTF-8 text
                if args.len() < 8 {
                    return Err(PluginError::Other("Invalid args for set_document".into()));
                }

                // This would be called by the host, not other plugins
                // Just acknowledge for now
                Ok(Vec::new())
            }

            _ => Err(PluginError::Other(format!("Unknown method: {}", method).into())),
        }
    }
}