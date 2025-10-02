//! Decoupled text rendering with separate layout and style
//!
//! Layout cache: positions (only changes on text edits)
//! Style buffer: token IDs (changes on syntax updates)
//! Palette: token → color mapping (instant theme switching)

use ahash::HashMap;
use std::ops::Range;
use tiny_core::{tree, DocTree as Tree};
use tiny_sdk::{LayoutPos, PhysicalPos};

/// Unified glyph with position and style data
#[derive(Clone, Debug)]
pub struct UnifiedGlyph {
    pub char: char,
    pub layout_pos: LayoutPos,     // Logical position
    pub physical_pos: PhysicalPos, // Physical pixels
    pub tex_coords: [f32; 4],      // Atlas coordinates
    pub char_byte_offset: usize,   // Byte position in document
    pub token_id: u16,             // Style token ID
    pub relative_pos: f32,         // Position within token (0.0-1.0)
}

// Legacy type alias for compatibility during migration
pub type GlyphPosition = UnifiedGlyph;

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

/// Syntax state with stable tokens and edit tracking
pub struct SyntaxState {
    /// Last completed tree-sitter parse
    pub stable_tokens: Vec<TokenRange>,
    /// Range of document that has been modified since last parse
    pub dirty_range: Option<Range<usize>>,
    /// Version of stable tokens
    pub stable_version: u64,
    /// Accumulated edit delta for adjusting token ranges
    /// Tracks (position, delta) for each edit since last fresh parse
    pub edit_deltas: Vec<(usize, isize)>,
}

/// Decoupled text renderer
pub struct TextRenderer {
    // === LAYOUT WITH INTEGRATED STYLE ===
    /// All glyphs with positions and styles
    pub layout_cache: Vec<UnifiedGlyph>,
    /// Line metadata for culling
    pub line_cache: Vec<LineInfo>,
    /// Document version for cache invalidation
    pub layout_version: u64,

    // === SYNTAX STATE ===
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

    // === WIDGET BOUNDS ===
    /// Editor widget bounds (where text should render)
    pub editor_bounds: Option<tiny_sdk::types::LayoutRect>,

    // === CACHE ===
    /// Cached flattened text to avoid re-flattening on every layout update
    cached_text: Option<std::sync::Arc<String>>,
    cached_text_version: u64,
}

impl TextRenderer {
    pub fn new() -> Self {
        Self {
            layout_cache: Vec::new(),
            line_cache: Vec::new(),
            layout_version: u64::MAX, // Force initial update
            syntax_state: SyntaxState {
                stable_tokens: Vec::new(),
                dirty_range: None,
                stable_version: 0,
                edit_deltas: Vec::new(),
            },
            visible_lines: 0..0,
            visible_chars: Vec::new(),
            gpu_style_buffer: None,
            palette_texture: None,
            editor_bounds: None,
            cached_text: None,
            cached_text_version: 0,
        }
    }

    /// Set the editor bounds for this renderer
    pub fn set_editor_bounds(&mut self, bounds: tiny_sdk::types::LayoutRect) {
        self.editor_bounds = Some(bounds);
    }

    /// Update layout cache when text changes
    pub fn update_layout(
        &mut self,
        tree: &Tree,
        font_system: &tiny_font::SharedFontSystem,
        viewport: &crate::coordinates::Viewport,
    ) {
        self.update_layout_internal(tree, font_system, viewport, false);
    }

    pub fn update_layout_internal(
        &mut self,
        tree: &Tree,
        font_system: &tiny_font::SharedFontSystem,
        viewport: &crate::coordinates::Viewport,
        force: bool,
    ) {
        // Only rebuild if text actually changed or forced
        if !force && tree.version == self.layout_version {
            return;
        }

        // Build map of (line, pos_in_line, char) → (token_id, relative_pos) for preservation
        // This keeps colors stable when layout rebuilds (e.g., after undo)
        let old_tokens: HashMap<(u32, u32, char), (u16, f32)> = self
            .layout_cache
            .iter()
            .enumerate()
            .filter_map(|(glyph_idx, g)| {
                if g.token_id == 0 {
                    return None; // Skip unstyled glyphs
                }
                // Find which line this glyph is on using binary search
                let line_idx = self
                    .line_cache
                    .binary_search_by(|l| {
                        if glyph_idx < l.char_range.start {
                            std::cmp::Ordering::Greater
                        } else if glyph_idx >= l.char_range.end {
                            std::cmp::Ordering::Less
                        } else {
                            std::cmp::Ordering::Equal
                        }
                    })
                    .ok()?;
                let line_info = self.line_cache.get(line_idx)?;
                let pos_in_line = (glyph_idx - line_info.char_range.start) as u32;
                Some((
                    (line_info.line_number, pos_in_line, g.char),
                    (g.token_id, g.relative_pos),
                ))
            })
            .collect();

        self.layout_cache.clear();
        self.line_cache.clear();

        // Use cached text if available and version matches
        let text_arc = if self.cached_text_version == tree.version && self.cached_text.is_some() {
            self.cached_text.clone().unwrap()
        } else {
            let new_text = tree.flatten_to_string();
            self.cached_text = Some(new_text.clone());
            self.cached_text_version = tree.version;
            new_text
        };

        let text = text_arc.as_str();
        let lines: Vec<&str> = text.lines().collect();

        let mut char_index = 0;
        let mut byte_offset = 0;

        // Use editor bounds if available, otherwise fall back to margins
        let (x_offset, mut y_pos) = if let Some(bounds) = &self.editor_bounds {
            (0.0, 0.0) // Local coordinates within bounds
        } else {
            (
                viewport.margin.x.0,
                viewport.global_margin.y.0 + viewport.margin.y.0,
            )
        };

        let text_len = text.len();

        for (line_idx, line_text) in lines.iter().enumerate() {
            let line_start_char = char_index;
            let line_start_byte = byte_offset;

            // Layout this line
            let layout = font_system.layout_text_scaled(
                line_text,
                viewport.metrics.font_size,
                viewport.scale_factor,
            );

            // Build a mapping from source text chars to their byte positions
            // This handles tabs which expand to multiple glyphs but occupy 1 byte
            let mut source_byte_offsets = Vec::new();
            let mut line_byte = line_start_byte;
            for ch in line_text.chars() {
                source_byte_offsets.push(line_byte);
                line_byte += ch.len_utf8();
            }

            // Track position in source text (NOT glyph index!)
            let mut source_char_idx = 0;

            // Add glyphs to cache
            for (glyph_idx, glyph) in layout.glyphs.iter().enumerate() {
                let layout_pos = LayoutPos::new(
                    x_offset + glyph.pos.x.0 / viewport.scale_factor,
                    y_pos + glyph.pos.y.0 / viewport.scale_factor,
                );

                // Get the byte offset from source text position
                // Font system may expand tabs to multiple space glyphs - they all map to the tab's byte
                let char_byte_offset = if source_char_idx < source_byte_offsets.len() {
                    source_byte_offsets[source_char_idx]
                } else {
                    byte_offset
                };

                // Try to preserve token from old layout
                let key = (
                    line_idx as u32,
                    (char_index - line_start_char) as u32,
                    glyph.char,
                );
                let (token_id, relative_pos) = old_tokens.get(&key).copied().unwrap_or((0, 0.0));

                self.layout_cache.push(UnifiedGlyph {
                    char: glyph.char,
                    layout_pos,
                    physical_pos: glyph.pos,
                    tex_coords: glyph.tex_coords,
                    char_byte_offset,
                    token_id,
                    relative_pos,
                });

                char_index += 1;

                // Advance source position only when we see a non-expanded glyph
                // Tabs expand to 4 spaces - only advance after we've seen all 4
                // Check if next source char is different from current glyph char
                if source_char_idx < line_text.chars().count() {
                    let source_char = line_text.chars().nth(source_char_idx).unwrap();
                    // If source is tab but glyph is space, we're in an expansion
                    if source_char == '\t' && glyph.char == ' ' {
                        // Check if we're at the last space of the tab expansion (tab width = 4)
                        let next_glyph_is_not_space = layout
                            .glyphs
                            .get(glyph_idx + 1)
                            .map(|g| g.char != ' ')
                            .unwrap_or(true);
                        if next_glyph_is_not_space {
                            source_char_idx += 1;
                        }
                    } else {
                        source_char_idx += 1;
                    }
                }
            }

            // Update byte_offset to end of line
            byte_offset = line_byte;

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
                let key = (line_idx as u32, (char_index - line_start_char) as u32, '\n');
                let (token_id, relative_pos) = old_tokens.get(&key).copied().unwrap_or((0, 0.0));
                self.layout_cache.push(UnifiedGlyph {
                    char: '\n',
                    layout_pos: LayoutPos::new(x_offset, y_pos),
                    physical_pos: PhysicalPos::new(
                        x_offset * viewport.scale_factor,
                        y_pos * viewport.scale_factor,
                    ),
                    tex_coords: [0.0, 0.0, 0.0, 0.0], // Invisible
                    char_byte_offset: byte_offset,
                    token_id,
                    relative_pos,
                });
                byte_offset += 1;
                char_index += 1;
            } else if text.ends_with('\n') {
                let key = (line_idx as u32, (char_index - line_start_char) as u32, '\n');
                let (token_id, relative_pos) = old_tokens.get(&key).copied().unwrap_or((0, 0.0));
                self.layout_cache.push(UnifiedGlyph {
                    char: '\n',
                    layout_pos: LayoutPos::new(x_offset, y_pos),
                    physical_pos: PhysicalPos::new(
                        x_offset * viewport.scale_factor,
                        y_pos * viewport.scale_factor,
                    ),
                    tex_coords: [0.0, 0.0, 0.0, 0.0], // Invisible
                    char_byte_offset: byte_offset,
                    token_id,
                    relative_pos,
                });
                byte_offset += 1;
                char_index += 1;
            }

            y_pos += viewport.metrics.line_height;
        }

        self.layout_version = tree.version;
    }

    /// Update style buffer with legacy token ranges
    pub fn update_syntax(&mut self, tokens: &[TokenRange], fresh_parse: bool) {
        // Early exit if no tokens - preserve existing highlights (don't clear!)
        // This prevents white text when waiting for fresh parse
        if tokens.is_empty() {
            if fresh_parse {
                // Fresh parse with no tokens means parse failed or doc is empty - clear
                for glyph in &mut self.layout_cache {
                    glyph.token_id = 0;
                    glyph.relative_pos = 0.0;
                }
                self.syntax_state.dirty_range = None;
            }
            // If not fresh_parse and no tokens, keep existing highlights
            return;
        }

        // Strategy for stable syntax highlighting:
        // - fresh_parse=true: Authoritative tokens, apply directly
        // - fresh_parse=false: Adjust old tokens for accumulated edits, then apply

        let adjusted_tokens: Vec<TokenRange>;
        let tokens_to_apply = if fresh_parse {
            self.syntax_state.dirty_range = None;
            self.syntax_state.edit_deltas.clear();

            // Store fresh tokens and increment version
            self.syntax_state.stable_tokens = tokens
                .iter()
                .map(|t| TokenRange {
                    byte_range: t.byte_range.clone(),
                    token_id: t.token_id as u8,
                })
                .collect();

            // Increment our version to match that we consumed a new parse
            self.syntax_state.stable_version += 1;

            tokens
        } else {
            // Early exit: if no edits have accumulated, use stable tokens as-is
            if self.syntax_state.edit_deltas.is_empty() {
                &self.syntax_state.stable_tokens
            } else {
                // Optimize: sort edit_deltas once and compute cumulative shifts
                // This reduces from O(tokens * edits) to O(edits log edits + tokens log edits)
                let mut sorted_edits = self.syntax_state.edit_deltas.clone();
                sorted_edits.sort_by_key(|&(pos, _)| pos);

                // Build cumulative delta array for efficient binary search
                // cumulative[i] = sum of all deltas for edits at position <= sorted_edits[i].0
                let mut cumulative_deltas: Vec<(usize, isize)> =
                    Vec::with_capacity(sorted_edits.len());
                let mut sum = 0;
                for &(pos, delta) in &sorted_edits {
                    sum += delta;
                    cumulative_deltas.push((pos, sum));
                }

                // Helper to find cumulative delta at a position using binary search
                let get_cumulative_at = |pos: usize| -> isize {
                    match cumulative_deltas.binary_search_by(|&(p, _)| p.cmp(&pos)) {
                        Ok(idx) => cumulative_deltas[idx].1,
                        Err(0) => 0, // No edits before this position
                        Err(idx) => cumulative_deltas[idx - 1].1,
                    }
                };

                // Adjust stable tokens using binary search
                adjusted_tokens = self
                    .syntax_state
                    .stable_tokens
                    .iter()
                    .map(|t| {
                        let original_start = t.byte_range.start;
                        let original_end = t.byte_range.end;

                        // Find cumulative delta for all edits <= start
                        // These shift the entire token
                        let shift_before_start = get_cumulative_at(original_start);

                        // Find cumulative delta for all edits < end
                        // The difference gives us edits within (start, end) that only shift the end
                        let shift_before_end = if original_end > 0 {
                            get_cumulative_at(original_end - 1)
                        } else {
                            0
                        };
                        let shift_within = shift_before_end - shift_before_start;

                        let new_start =
                            ((original_start as isize) + shift_before_start).max(0) as usize;
                        let new_end = ((original_end as isize) + shift_before_start + shift_within)
                            .max(new_start as isize) as usize;

                        TokenRange {
                            byte_range: new_start..new_end,
                            token_id: t.token_id,
                        }
                    })
                    .collect();
                &adjusted_tokens
            }
        };

        // Clear all tokens before applying
        for glyph in &mut self.layout_cache {
            glyph.token_id = 0;
            glyph.relative_pos = 0.0;
        }

        // Apply tokens - O(n + m) single-pass merge
        let mut glyph_idx = 0;
        let mut token_idx = 0;

        // Single-pass merge: both glyphs and tokens are sorted by byte position
        while glyph_idx < self.layout_cache.len() && token_idx < tokens_to_apply.len() {
            let glyph_pos = self.layout_cache[glyph_idx].char_byte_offset;
            let token = &tokens_to_apply[token_idx];

            if glyph_pos < token.byte_range.start {
                // Glyph is before current token - leave as default (0)
                glyph_idx += 1;
            } else if glyph_pos >= token.byte_range.end {
                // Glyph is after current token - advance to next token
                token_idx += 1;
            } else {
                // Glyph is within current token - apply styling
                self.layout_cache[glyph_idx].token_id = token.token_id as u16;

                // Calculate relative position within token
                let token_byte_length = token.byte_range.end - token.byte_range.start;
                let byte_offset_in_token = glyph_pos - token.byte_range.start;
                self.layout_cache[glyph_idx].relative_pos = if token_byte_length > 0 {
                    (byte_offset_in_token as f32) / (token_byte_length as f32)
                } else {
                    0.0
                };

                glyph_idx += 1;
            }
        }
    }

    /// Infer style from surrounding context for dirty regions
    fn infer_style_from_context(&self, glyph_idx: usize) -> u16 {
        // Simple heuristic: look for the style of the previous non-whitespace character
        // This helps maintain visual continuity when typing within a token

        // Look backwards for a styled character
        for i in (0..glyph_idx).rev() {
            if let Some(glyph) = self.layout_cache.get(i) {
                // Skip whitespace when looking for context
                if !glyph.char.is_whitespace() && glyph.token_id != 0 {
                    return glyph.token_id;
                }
            }
        }

        // If no context found, look forward
        for i in glyph_idx + 1..self.layout_cache.len().min(glyph_idx + 10) {
            if let Some(glyph) = self.layout_cache.get(i) {
                if !glyph.char.is_whitespace() && glyph.token_id != 0 {
                    return glyph.token_id;
                }
            }
        }

        // No context found - return default (no highlighting)
        0
    }

    /// Handle incremental syntax update (while tree-sitter is parsing)
    pub fn apply_incremental_edit(&mut self, edit: &tree::Edit) {
        // Track the edit delta for token range adjustment
        let (pos, delta) = match edit {
            tree::Edit::Insert { pos, content } => {
                let len = match content {
                    tree::Content::Text(text) => text.len(),
                    tree::Content::Spatial(_) => 0,
                };
                (*pos, len as isize)
            }
            tree::Edit::Delete { range } => (range.start, -(range.len() as isize)),
            tree::Edit::Replace { range, content } => {
                let old_len = range.len();
                let new_len = match content {
                    tree::Content::Text(text) => text.len(),
                    tree::Content::Spatial(_) => 0,
                };
                (range.start, (new_len as isize) - (old_len as isize))
            }
        };

        // Store the edit delta for later adjustment
        self.syntax_state.edit_deltas.push((pos, delta));

        // Calculate the affected range for this edit
        let edit_range = match edit {
            tree::Edit::Insert { pos, content } => {
                let len = match content {
                    tree::Content::Text(text) => text.len(),
                    tree::Content::Spatial(_) => 0,
                };
                *pos..*pos + len
            }
            tree::Edit::Delete { range } => range.start..range.start,
            tree::Edit::Replace { range, content } => {
                let len = match content {
                    tree::Content::Text(text) => text.len(),
                    tree::Content::Spatial(_) => 0,
                };
                range.start..range.start + len
            }
        };

        // Expand the dirty range to include this edit
        self.syntax_state.dirty_range = match self.syntax_state.dirty_range.take() {
            None => Some(edit_range),
            Some(existing) => {
                Some(existing.start.min(edit_range.start)..existing.end.max(edit_range.end))
            }
        };
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
    pub fn get_visible_glyphs(&self) -> Vec<(UnifiedGlyph, u16)> {
        self.visible_chars
            .iter()
            .filter_map(|&idx| {
                let glyph = self.layout_cache.get(idx)?;
                Some((glyph.clone(), glyph.token_id))
            })
            .collect()
    }

    /// Get visible glyphs with full style information
    pub fn get_visible_glyphs_with_style(&self) -> Vec<UnifiedGlyph> {
        self.visible_chars
            .iter()
            .filter_map(|&idx| self.layout_cache.get(idx).cloned())
            .collect()
    }

    /// Upload style buffer to GPU
    pub fn upload_style_buffer(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        // Extract token IDs from unified glyphs
        let style_buffer: Vec<u16> = self.layout_cache.iter().map(|g| g.token_id).collect();

        let buffer_size = (style_buffer.len() * 2) as u64; // u16 per character

        // Create or recreate buffer if size changed
        if self
            .gpu_style_buffer
            .as_ref()
            .map(|b| b.size() != buffer_size)
            .unwrap_or(true)
        {
            self.gpu_style_buffer = Some(device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("Style Buffer"),
                size: buffer_size,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }

        if let Some(buffer) = &self.gpu_style_buffer {
            queue.write_buffer(buffer, 0, bytemuck::cast_slice(&style_buffer));
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
        let max_colors = theme1
            .max_colors_per_token
            .max(theme2.max_colors_per_token)
            .max(1);
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
