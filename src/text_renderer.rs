//! Decoupled text rendering with separate layout and style
//!
//! Layout cache: positions (only changes on text edits)
//! Style buffer: token IDs (changes on syntax updates)
//! Palette: token → color mapping (instant theme switching)

use std::io::BufRead;
use crate::coordinates::{LayoutPos, PhysicalPos};
use crate::tree::Tree;
use std::ops::Range;

/// Stable glyph position in layout cache
#[derive(Clone, Debug)]
pub struct GlyphPosition {
    pub char: char,
    pub layout_pos: LayoutPos,     // Logical position
    pub physical_pos: PhysicalPos, // Physical pixels
    pub tex_coords: [f32; 4],      // Atlas coordinates
    pub char_byte_offset: usize,   // Byte position in document
}

/// Glyph with full style data for rendering
#[derive(Clone, Debug)]
pub struct GlyphStyleData {
    pub glyph_pos: GlyphPosition,
    pub token_id: u8,
    pub relative_pos: f32,
}

/// Line information for efficient culling
#[derive(Clone, Debug)]
pub struct LineInfo {
    pub line_number: u32,
    pub byte_range: Range<usize>,
    pub char_range: Range<usize>, // Character indices into layout_cache
    pub y_position: f32,          // Layout Y coordinate
    pub height: f32,
}

/// Token range with token ID
#[derive(Clone, Debug)]
pub struct TokenRange {
    pub byte_range: Range<usize>,
    pub token_id: u8,
}

/// Legacy token range for compatibility
#[derive(Clone, Debug)]
pub struct LegacyTokenRange {
    pub byte_range: Range<usize>,
    pub token_id: u16,
}

/// Syntax state with stable + incremental tokens
pub struct SyntaxState {
    /// Last completed tree-sitter parse
    pub stable_tokens: Vec<TokenRange>,
    /// Context-based temporary tokens for new text
    pub incremental_tokens: Vec<TokenRange>,
    /// Edits since last stable parse - used to shift token ranges
    pub pending_edits: Vec<crate::tree::Edit>,
    /// Version of stable tokens
    pub stable_version: u64,
}

/// Decoupled text renderer
pub struct TextRenderer {
    // === LAYOUT (stable, only changes on text edits) ===
    /// All glyph positions
    pub layout_cache: Vec<GlyphPosition>,
    /// Line metadata for culling
    pub line_cache: Vec<LineInfo>,
    /// Document version for cache invalidation
    pub layout_version: u64,

    // === STYLE (changes on syntax updates) ===
    /// Per-character token IDs (parallel to layout_cache)
    pub style_buffer: Vec<u16>,
    /// Per-character relative positions within token (0.0-1.0)
    pub relative_pos_buffer: Vec<f32>,
    /// Syntax state
    pub syntax_state: SyntaxState,

    // === CULLING ===
    /// Currently visible lines
    pub visible_lines: Range<u32>,
    /// Currently visible character indices
    pub visible_chars: Vec<usize>,

    // === GPU RESOURCES ===
    /// Style buffer on GPU (u16 per character)
    pub gpu_style_buffer: Option<wgpu::Buffer>,
    /// Palette texture (256 colors, RGBA8)
    pub palette_texture: Option<wgpu::Texture>,

}

impl TextRenderer {
    pub fn new() -> Self {
        Self {
            layout_cache: Vec::new(),
            line_cache: Vec::new(),
            layout_version: u64::MAX, // Force initial update
            style_buffer: Vec::new(),
            relative_pos_buffer: Vec::new(),
            syntax_state: SyntaxState {
                stable_tokens: Vec::new(),
                incremental_tokens: Vec::new(),
                pending_edits: Vec::new(),
                stable_version: 0,
            },
            visible_lines: 0..0,
            visible_chars: Vec::new(),
            gpu_style_buffer: None,
            palette_texture: None,
        }
    }

    /// Update layout cache when text changes
    pub fn update_layout(
        &mut self,
        tree: &Tree,
        font_system: &crate::font::SharedFontSystem,
        viewport: &crate::coordinates::Viewport,
    ) {
        // Only rebuild if text actually changed
        if tree.version == self.layout_version {
            return;
        }

        self.layout_cache.clear();
        self.line_cache.clear();

        let text = tree.flatten_to_string();
        let lines: Vec<&str> = text.lines().collect();

        let mut char_index = 0;
        let mut byte_offset = 0;
        let mut y_pos = viewport.margin.y.0;

        for (line_idx, line_text) in lines.iter().enumerate() {
            let line_start_char = char_index;
            let line_start_byte = byte_offset;

            // Layout this line
            let layout = font_system.layout_text_scaled(
                line_text,
                viewport.metrics.font_size,
                viewport.scale_factor,
            );

            // Add glyphs to cache
            for glyph in layout.glyphs {
                let layout_pos = LayoutPos::new(
                    viewport.margin.x.0 + glyph.pos.x.0 / viewport.scale_factor,
                    y_pos + glyph.pos.y.0 / viewport.scale_factor,
                );

                self.layout_cache.push(GlyphPosition {
                    char: glyph.char,
                    layout_pos,
                    physical_pos: glyph.pos,
                    tex_coords: glyph.tex_coords,
                    char_byte_offset: byte_offset,
                });

                byte_offset += glyph.char.len_utf8();
                char_index += 1;
            }

            // Add line info
            self.line_cache.push(LineInfo {
                line_number: line_idx as u32,
                byte_range: line_start_byte..byte_offset,
                char_range: line_start_char..char_index,
                y_position: y_pos,
                height: viewport.metrics.line_height,
            });

            // Add newline as a glyph (invisible but maintains byte position)
            if line_idx < lines.len() - 1 {
                // Newline between lines
                self.layout_cache.push(GlyphPosition {
                    char: '\n',
                    layout_pos: LayoutPos::new(viewport.margin.x.0, y_pos),
                    physical_pos: PhysicalPos::new(viewport.margin.x.0 * viewport.scale_factor, y_pos * viewport.scale_factor),
                    tex_coords: [0.0, 0.0, 0.0, 0.0], // Invisible
                    char_byte_offset: byte_offset,
                });
                byte_offset += 1;
                char_index += 1;
            } else if text.ends_with('\n') {
                // Trailing newline
                self.layout_cache.push(GlyphPosition {
                    char: '\n',
                    layout_pos: LayoutPos::new(viewport.margin.x.0, y_pos),
                    physical_pos: PhysicalPos::new(viewport.margin.x.0 * viewport.scale_factor, y_pos * viewport.scale_factor),
                    tex_coords: [0.0, 0.0, 0.0, 0.0], // Invisible
                    char_byte_offset: byte_offset,
                });
                byte_offset += 1;
                char_index += 1;
            }

            y_pos += viewport.metrics.line_height;
        }

        self.layout_version = tree.version;

        // Style buffers need to match layout cache size
        self.style_buffer.resize(self.layout_cache.len(), 0);
        self.relative_pos_buffer.resize(self.layout_cache.len(), 0.0);
    }

    /// Update syntax buffer with syntax highlighting from tokens
    pub fn update_syntax_from_tokens(&mut self, tokens: &[TokenRange], fresh_parse: bool) {
        // Convert to legacy format for now
        let legacy_tokens: Vec<LegacyTokenRange> = tokens
            .iter()
            .map(|t| LegacyTokenRange {
                byte_range: t.byte_range.clone(),
                token_id: t.token_id as u16,
            })
            .collect();

        self.update_syntax(&legacy_tokens, fresh_parse);
    }

    /// Update style buffer with legacy token ranges
    pub fn update_syntax(&mut self, tokens: &[LegacyTokenRange], fresh_parse: bool) {
        // Early exit if no tokens - just clear style buffer and return
        if tokens.is_empty() {
            self.style_buffer.fill(0);
            if fresh_parse {
                self.syntax_state.pending_edits.clear();
            }
            return;
        }

        // If this is a fresh parse, clear pending edits
        if fresh_parse {
            self.syntax_state.pending_edits.clear();
        }

        // Clear style buffers
        self.style_buffer.fill(0);
        self.relative_pos_buffer.fill(0.0);

        // Shift token ranges based on pending edits
        // The tokens are from OLD text, but layout is from NEW text
        let shifted_tokens: Vec<LegacyTokenRange> = if self.syntax_state.pending_edits.is_empty() {
            tokens.to_vec()
        } else {
            // Track cumulative offset as we apply edits
            // IMPORTANT: All edits have positions relative to the ORIGINAL text
            // So we need to sort them by position and track cumulative effect
            let mut sorted_edits = self.syntax_state.pending_edits.clone();
            sorted_edits.sort_by_key(|edit| {
                match edit {
                    crate::tree::Edit::Insert { pos, .. } => *pos,
                    crate::tree::Edit::Delete { range } => range.start,
                    crate::tree::Edit::Replace { range, .. } => range.start,
                }
            });

            tokens
                .iter()
                .map(|token| {
                    let mut range = token.byte_range.clone();
                    let mut cumulative_offset: i32 = 0;

                    // Apply edits in sorted order, tracking cumulative offset
                    for edit in &sorted_edits {
                        match edit {
                            crate::tree::Edit::Insert { pos, content } => {
                                let insert_len = if let crate::tree::Content::Text(text) = content {
                                    text.len() as i32
                                } else {
                                    0
                                };

                                // Apply the edit at the ORIGINAL position
                                if range.start >= *pos {
                                    // Token starts after this edit - shift it
                                    range.start = (range.start as i32 + insert_len) as usize;
                                    range.end = (range.end as i32 + insert_len) as usize;
                                } else if range.end > *pos {
                                    // Token spans this edit - extend its end
                                    range.end = (range.end as i32 + insert_len) as usize;
                                }
                            }
                            crate::tree::Edit::Delete { range: del_range } => {
                                let delete_len = (del_range.end - del_range.start) as i32;

                                // Apply the delete at the ORIGINAL position
                                if range.start >= del_range.end {
                                    // Token starts after deletion
                                    range.start = range.start.saturating_sub(delete_len as usize);
                                    range.end = range.end.saturating_sub(delete_len as usize);
                                } else if range.start < del_range.start && range.end > del_range.end {
                                    // Token fully contains the deletion
                                    range.end = range.end.saturating_sub(delete_len as usize);
                                } else if range.start >= del_range.start && range.end <= del_range.end {
                                    // Token is fully within deletion - mark as invalid
                                    range.start = 0;
                                    range.end = 0;
                                } else if range.end > del_range.start && range.end <= del_range.end {
                                    // Token partially overlaps deletion start
                                    range.end = del_range.start;
                                }
                            }
                            crate::tree::Edit::Replace { range: repl_range, content } => {
                                let delete_len = (repl_range.end - repl_range.start) as i32;
                                let insert_len = if let crate::tree::Content::Text(text) = content {
                                    text.len() as i32
                                } else {
                                    0
                                };
                                let net_change = insert_len - delete_len;

                                if range.start >= repl_range.end {
                                    // Token is after replacement
                                    if net_change > 0 {
                                        range.start = (range.start as i32 + net_change) as usize;
                                        range.end = (range.end as i32 + net_change) as usize;
                                    } else {
                                        range.start = range.start.saturating_sub((-net_change) as usize);
                                        range.end = range.end.saturating_sub((-net_change) as usize);
                                    }
                                } else if range.end > repl_range.start {
                                    // Token overlaps with replacement
                                    if net_change > 0 {
                                        range.end = (range.end as i32 + net_change) as usize;
                                    } else {
                                        range.end = range.end.saturating_sub((-net_change) as usize);
                                    }
                                }
                            }
                        }
                    }

                    LegacyTokenRange {
                        byte_range: range,
                        token_id: token.token_id,
                    }
                })
                .filter(|t| t.byte_range.start < t.byte_range.end) // Filter out invalid tokens
                .collect()
        };

        // Apply shifted token ranges

        // Apply tokens using binary search - O(T * log G) instead of O(T * G)
        for token in &shifted_tokens {
            // Find first glyph >= token.byte_range.start
            let start_idx = self.layout_cache
                .binary_search_by_key(&token.byte_range.start, |glyph| glyph.char_byte_offset)
                .unwrap_or_else(|i| i);

            // Find first glyph >= token.byte_range.end
            let end_idx = self.layout_cache
                .binary_search_by_key(&token.byte_range.end, |glyph| glyph.char_byte_offset)
                .unwrap_or_else(|i| i);

            // Apply token to all glyphs in range [start_idx, end_idx)
            let token_byte_length = token.byte_range.end - token.byte_range.start;

            for i in start_idx..end_idx.min(self.style_buffer.len()) {
                self.style_buffer[i] = token.token_id as u16;

                // Calculate relative position within the coalesced token (0.0 to 1.0)
                if let Some(glyph) = self.layout_cache.get(i) {
                    let byte_offset_in_token = glyph.char_byte_offset - token.byte_range.start;
                    let relative_pos = if token_byte_length > 0 {
                        (byte_offset_in_token as f32) / (token_byte_length as f32)
                    } else {
                        0.0
                    };
                    self.relative_pos_buffer[i] = relative_pos;
                }
            }
        }

        // DON'T clear pending edits! We need to keep accumulating them
        // until tree-sitter actually re-parses with the new text
        // self.syntax_state.pending_edits.clear();
    }

    /// Handle incremental syntax update (while tree-sitter is parsing)
    pub fn apply_incremental_edit(&mut self, edit: &crate::tree::Edit) {
        println!("[DEBUG] apply_incremental_edit called with: {:?}", edit);

        // Debug: Show all pending edits when we have multiple
        // if !self.syntax_state.pending_edits.is_empty() {
        //     println!("[DEBUG] Existing pending edits:");
        //     for (i, e) in self.syntax_state.pending_edits.iter().enumerate() {
        //         println!("  [{}]: {:?}", i, e);
        //     }
        // }

        // Store edit for later reconciliation
        self.syntax_state.pending_edits.push(edit.clone());
        // println!("[DEBUG] Pending edits count: {}", self.syntax_state.pending_edits.len());
    }

    /// Expand tabs to spaces for consistent glyph mapping
    fn expand_tabs_to_spaces(&self, line_text: &str, tab_stops: u32) -> String {
        let mut expanded = String::new();
        let mut column = 0;

        for ch in line_text.chars() {
            if ch == '\t' {
                // Calculate spaces needed to reach next tab stop
                let spaces_needed = tab_stops as usize - (column % tab_stops as usize);
                expanded.push_str(&" ".repeat(spaces_needed));
                column += spaces_needed;
            } else {
                expanded.push(ch);
                column += 1;
            }
        }

        expanded
    }

    /// Find token at byte position (for context inheritance)
    fn find_token_at(&self, byte_pos: usize) -> Option<u8> {
        // Check stable tokens first
        for token in &self.syntax_state.stable_tokens {
            if token.byte_range.contains(&byte_pos) {
                return Some(token.token_id);
            }
        }

        // Check incremental tokens
        for token in &self.syntax_state.incremental_tokens {
            if token.byte_range.contains(&byte_pos) {
                return Some(token.token_id);
            }
        }

        None
    }

    /// Update visible range for culling
    pub fn update_visible_range(&mut self, viewport: &crate::coordinates::Viewport, tree: &Tree) {
        let visible_byte_range = viewport.visible_byte_range_with_tree(tree);

        // Find visible lines
        let mut start_line = None;
        let mut end_line = None;

        for (i, line) in self.line_cache.iter().enumerate() {
            if line.byte_range.end > visible_byte_range.start && start_line.is_none() {
                start_line = Some(i as u32);
            }
            if line.byte_range.start < visible_byte_range.end {
                end_line = Some(i as u32 + 1);
            }
        }

        self.visible_lines = start_line.unwrap_or(0)..end_line.unwrap_or(0);

        // Find visible characters
        self.visible_chars.clear();
        for line_idx in self.visible_lines.clone() {
            if let Some(line) = self.line_cache.get(line_idx as usize) {
                for char_idx in line.char_range.clone() {
                    self.visible_chars.push(char_idx);
                }
            }
        }
    }

    /// Get visible glyphs with their token IDs
    pub fn get_visible_glyphs(&self) -> Vec<(GlyphPosition, u16)> {
        self.visible_chars
            .iter()
            .filter_map(|&idx| {
                let glyph = self.layout_cache.get(idx)?;
                let token_id = self.style_buffer.get(idx).copied().unwrap_or(0);
                Some((glyph.clone(), token_id))
            })
            .collect()
    }

    /// Get visible glyphs with full style information
    pub fn get_visible_glyphs_with_style(&self) -> Vec<GlyphStyleData> {
        self.visible_chars
            .iter()
            .filter_map(|&idx| {
                let glyph = self.layout_cache.get(idx)?;
                let token_id = self.style_buffer.get(idx).copied().unwrap_or(0) as u8;
                let relative_pos = self.relative_pos_buffer.get(idx).copied().unwrap_or(0.0);
                Some(GlyphStyleData {
                    glyph_pos: glyph.clone(),
                    token_id,
                    relative_pos,
                })
            })
            .collect()
    }

    /// Upload style buffer to GPU
    pub fn upload_style_buffer(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        let buffer_size = (self.style_buffer.len() * 2) as u64; // u16 per character

        // Create or recreate buffer if size changed
        if self.gpu_style_buffer.as_ref().map(|b| b.size() != buffer_size).unwrap_or(true) {
            self.gpu_style_buffer = Some(device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("Style Buffer"),
                size: buffer_size,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }

        if let Some(buffer) = &self.gpu_style_buffer {
            queue.write_buffer(buffer, 0, bytemuck::cast_slice(&self.style_buffer));
        }
    }

    /// Create palette texture from theme
    pub fn create_palette_texture_from_theme(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        theme: &crate::theme::Theme,
    ) {
        let texture_data = theme.generate_texture_data();
        let height = theme.max_colors_per_token.max(1) as u32;

        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Theme Palette Texture"),
            size: wgpu::Extent3d {
                width: 256,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &texture_data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(256 * 4),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width: 256,
                height,
                depth_or_array_layers: 1,
            },
        );

        self.palette_texture = Some(texture);
    }

    /// Create palette texture for theme interpolation
    pub fn create_interpolation_texture(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        theme1: &crate::theme::Theme,
        theme2: &crate::theme::Theme,
    ) {
        let texture_data = crate::theme::Theme::merge_for_interpolation(theme1, theme2);
        let max_colors = theme1.max_colors_per_token.max(theme2.max_colors_per_token).max(1);
        let height = (max_colors * 2) as u32; // Two themes stacked

        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Theme Interpolation Texture"),
            size: wgpu::Extent3d {
                width: 256,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &texture_data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(256 * 4),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width: 256,
                height,
                depth_or_array_layers: 1,
            },
        );

        self.palette_texture = Some(texture);
    }
}

/// Convert token type to ID for palette lookup
pub fn token_type_to_id(token: crate::syntax::TokenType) -> u16 {
    // Use the centralized function from syntax.rs
    crate::syntax::SyntaxHighlighter::token_type_to_id(token) as u16
}