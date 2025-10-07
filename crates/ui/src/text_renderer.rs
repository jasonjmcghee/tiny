//! Decoupled text rendering with separate layout and style
//!
//! Layout cache: positions (only changes on text edits)
//! Style buffer: token IDs (changes on syntax updates)
//! Palette: token → color mapping (instant theme switching)

use ahash::HashMap;
use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};
use tiny_core::{tree, DocTree as Tree};
use tiny_sdk::{LayoutPos, PhysicalPos};

/// Global flag for demo styles mode (for testing only)
pub static DEMO_STYLES_MODE: AtomicBool = AtomicBool::new(false);

/// Unified glyph with position and style data
#[derive(Clone, Debug)]
pub struct UnifiedGlyph {
    pub char: char,
    pub layout_pos: LayoutPos,     // Logical position
    pub physical_pos: PhysicalPos, // Physical pixels
    pub physical_width: f32,       // Glyph advance width in physical pixels
    pub tex_coords: [f32; 4],      // Atlas coordinates
    pub char_byte_offset: usize,   // Byte position in document
    pub token_id: u16,             // Style token ID
    pub relative_pos: f32,         // Position within token (0.0-1.0)
    pub atlas_index: u8,           // 0 = monochrome (R8), 1 = color (RGBA8)
    // Style attributes (populated from theme during syntax highlighting)
    pub weight: f32,         // Font weight (400 = normal, 700 = bold)
    pub italic: bool,        // Italic flag
    pub underline: bool,     // Underline decoration
    pub strikethrough: bool, // Strikethrough decoration
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

/// Cached shaping result for a single line
#[derive(Clone)]
struct ShapedLineCache {
    glyphs: Vec<tiny_font::PositionedGlyph>,
    cluster_map: tiny_font::ClusterMap,
}

/// Parse a demo style tag like "w700", "italic", "w700+italic+underline"
/// Returns (weight, italic, underline, strikethrough)
fn parse_demo_tag(tag: &str) -> (f32, bool, bool, bool) {
    let mut weight = 400.0;
    let mut italic = false;
    let mut underline = false;
    let mut strikethrough = false;

    for part in tag.split('+') {
        let part = part.trim();
        if part.starts_with('w') {
            // Weight like "w700"
            if let Ok(w) = part[1..].parse::<f32>() {
                weight = w;
            }
        } else if part == "italic" {
            italic = true;
        } else if part == "roman" {
            italic = false;
        } else if part == "underline" {
            underline = true;
        } else if part == "strike" {
            strikethrough = true;
        } else if part == "both" {
            underline = true;
            strikethrough = true;
        } else if part == "plain" {
            underline = false;
            strikethrough = false;
        }
    }

    (weight, italic, underline, strikethrough)
}

/// Decoupled text renderer
pub struct TextRenderer {
    // === LAYOUT WITH INTEGRATED STYLE ===
    /// All glyphs with positions and styles
    pub layout_cache: Vec<UnifiedGlyph>,
    /// Line metadata for culling
    pub line_cache: Vec<LineInfo>,
    /// Cache invalidation version (increments on any layout change)
    pub layout_version: u64,
    /// Last tree version we built layout from (tracks document content)
    last_tree_version: u64,
    /// Last metrics we built layout with (for auto-invalidation on metric changes)
    last_font_size: f32,
    last_line_height: f32,
    last_scale_factor: f32,
    /// Cluster map for ligature-aware cursor positioning
    pub cluster_maps: Vec<tiny_font::ClusterMap>,

    // === SYNTAX STATE ===
    /// Syntax state
    pub syntax_state: SyntaxState,
    /// Syntax highlighter for this renderer (per-tab)
    pub syntax_highlighter: Option<std::sync::Arc<crate::syntax::SyntaxHighlighter>>,

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

    // === SHAPING CACHE ===
    /// Per-line shaping cache: (line_text_hash, font_size_bits, scale_factor_bits) -> shaped result
    /// Avoids expensive OpenType shaping for unchanged lines
    shaping_cache: HashMap<(u64, u32, u32), ShapedLineCache>,
}

impl TextRenderer {
    pub fn new() -> Self {
        Self {
            layout_cache: Vec::new(),
            line_cache: Vec::new(),
            layout_version: 0,
            last_tree_version: u64::MAX, // Force initial update (no tree version will match)
            last_font_size: 0.0,
            last_line_height: 0.0,
            last_scale_factor: 0.0,
            cluster_maps: Vec::new(),
            syntax_state: SyntaxState {
                stable_tokens: Vec::new(),
                dirty_range: None,
                stable_version: 0,
                edit_deltas: Vec::new(),
            },
            syntax_highlighter: None,
            visible_lines: 0..0,
            visible_chars: Vec::new(),
            gpu_style_buffer: None,
            palette_texture: None,
            shaping_cache: HashMap::default(),
        }
    }

    /// Update layout cache when text changes
    /// Outputs glyphs in canonical (0,0)-relative positions
    /// Automatically detects changes in font_size, line_height, scale_factor and rebuilds
    pub fn update_layout(
        &mut self,
        tree: &Tree,
        font_system: &tiny_font::SharedFontSystem,
        viewport: &crate::coordinates::Viewport,
        force: bool,
    ) {
        // Auto-detect metrics changes (font size, line height, scale factor)
        let metrics_changed = (viewport.metrics.font_size - self.last_font_size).abs() > 0.01
            || (viewport.metrics.line_height - self.last_line_height).abs() > 0.01
            || (viewport.scale_factor - self.last_scale_factor).abs() > 0.01;

        // Rebuild if text changed, metrics changed, or forced
        if !force && tree.version == self.last_tree_version && !metrics_changed {
            return;
        }

        // Build map of (line, pos_in_line, char) → (token_id, relative_pos, weight, italic, underline, strikethrough) for preservation
        // This keeps colors stable when layout rebuilds (e.g., after undo)
        let old_tokens: HashMap<(u32, u32, char), (u16, f32, f32, bool, bool, bool)> = self
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
                    (
                        g.token_id,
                        g.relative_pos,
                        g.weight,
                        g.italic,
                        g.underline,
                        g.strikethrough,
                    ),
                ))
            })
            .collect();

        self.layout_cache.clear();
        self.line_cache.clear();
        self.cluster_maps.clear();

        // Get text from tree (tree handles caching internally)
        let text = tree.flatten_to_string();
        let text = text.as_str();
        let lines: Vec<&str> = text.lines().collect();

        let mut char_index = 0;
        let mut byte_offset = 0;

        // Always start at (0, 0) - canonical positions only
        // TextView will apply bounds offset and scroll when rendering
        let x_offset = 0.0;
        let mut y_pos = 0.0;

        for (line_idx, line_text) in lines.iter().enumerate() {
            let line_start_char = char_index;
            let line_start_byte = byte_offset;

            // Compute cache key for this line
            let line_hash = {
                use std::hash::{Hash, Hasher};
                let mut hasher = ahash::AHasher::default();
                line_text.hash(&mut hasher);
                hasher.finish()
            };
            let font_size_bits = viewport.metrics.font_size.to_bits();
            let scale_factor_bits = viewport.scale_factor.to_bits();
            let cache_key = (line_hash, font_size_bits, scale_factor_bits);

            // Check cache first
            let (layout_glyphs, cluster_map) =
                if let Some(cached) = self.shaping_cache.get(&cache_key) {
                    // Cache hit - use cached shaped glyphs
                    (cached.glyphs.clone(), cached.cluster_map.clone())
                } else {
                    // Cache miss - shape the line and store result
                    let tiny_font::ShapedTextLayout {
                        glyphs: shaped_glyphs,
                        cluster_map: shaped_cluster_map,
                        ..
                    } = font_system.layout_text_shaped_with_tabs(
                        line_text,
                        viewport.metrics.font_size,
                        viewport.scale_factor,
                        None, // Use default shaping options
                    );

                    // Store in cache (limit cache size to prevent unbounded growth)
                    const MAX_CACHE_SIZE: usize = 10000;
                    if self.shaping_cache.len() < MAX_CACHE_SIZE {
                        self.shaping_cache.insert(
                            cache_key,
                            ShapedLineCache {
                                glyphs: shaped_glyphs.clone(),
                                cluster_map: shaped_cluster_map.clone(),
                            },
                        );
                    }

                    (shaped_glyphs, shaped_cluster_map)
                };

            // Store the cluster map for this line
            self.cluster_maps.push(cluster_map);

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
            for (glyph_idx, glyph) in layout_glyphs.iter().enumerate() {
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
                let (token_id, relative_pos, weight, italic, underline, strikethrough) = old_tokens
                    .get(&key)
                    .copied()
                    .unwrap_or((0, 0.0, 400.0, false, false, false));

                self.layout_cache.push(UnifiedGlyph {
                    char: glyph.char,
                    layout_pos,
                    physical_pos: glyph.pos.clone(),
                    physical_width: glyph.size.width.0,
                    tex_coords: glyph.tex_coords,
                    char_byte_offset,
                    token_id,
                    relative_pos,
                    atlas_index: glyph.atlas_index,
                    weight,
                    italic,
                    underline,
                    strikethrough,
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
                        let next_glyph_is_not_space = layout_glyphs
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
                let (token_id, relative_pos, weight, italic, underline, strikethrough) = old_tokens
                    .get(&key)
                    .copied()
                    .unwrap_or((0, 0.0, 400.0, false, false, false));
                self.layout_cache.push(UnifiedGlyph {
                    char: '\n',
                    layout_pos: LayoutPos::new(x_offset, y_pos),
                    physical_pos: PhysicalPos::new(
                        x_offset * viewport.scale_factor,
                        y_pos * viewport.scale_factor,
                    ),
                    physical_width: 0.0,              // Newlines have no width
                    tex_coords: [0.0, 0.0, 0.0, 0.0], // Invisible
                    char_byte_offset: byte_offset,
                    token_id,
                    relative_pos,
                    atlas_index: 0, // Invisible glyph, no atlas needed
                    weight,
                    italic,
                    underline,
                    strikethrough,
                });
                byte_offset += 1;
                char_index += 1;
            } else if text.ends_with('\n') {
                let key = (line_idx as u32, (char_index - line_start_char) as u32, '\n');
                let (token_id, relative_pos, weight, italic, underline, strikethrough) = old_tokens
                    .get(&key)
                    .copied()
                    .unwrap_or((0, 0.0, 400.0, false, false, false));
                self.layout_cache.push(UnifiedGlyph {
                    char: '\n',
                    layout_pos: LayoutPos::new(x_offset, y_pos),
                    physical_pos: PhysicalPos::new(
                        x_offset * viewport.scale_factor,
                        y_pos * viewport.scale_factor,
                    ),
                    physical_width: 0.0,              // Newlines have no width
                    tex_coords: [0.0, 0.0, 0.0, 0.0], // Invisible
                    char_byte_offset: byte_offset,
                    token_id,
                    relative_pos,
                    atlas_index: 0, // Invisible glyph, no atlas needed
                    weight,
                    italic,
                    underline,
                    strikethrough,
                });
                byte_offset += 1;
                char_index += 1;
            }

            y_pos += viewport.metrics.line_height;
        }

        // Track which tree version and metrics we built layout from
        self.last_tree_version = tree.version;
        self.last_font_size = viewport.metrics.font_size;
        self.last_line_height = viewport.metrics.line_height;
        self.last_scale_factor = viewport.scale_factor;

        // Always increment layout_version when we rebuild (for cache invalidation)
        self.layout_version = self.layout_version.wrapping_add(1);

        // Apply demo styles if enabled (parse [w700] tags and reshape)
        // This modifies glyphs in-place but doesn't require another version bump
        if DEMO_STYLES_MODE.load(Ordering::Relaxed) {
            self.apply_demo_styles(tree);
            self.reshape_for_styles(tree, font_system, viewport);
        }
    }

    /// Reshape lines that have mixed weight/italic based on glyph attributes
    /// Called after update_syntax_with_theme assigns token_ids and style attributes
    ///
    /// This is needed for markdown bold/italic, theme-specified weights (headings=900, bold=700), etc.
    pub fn reshape_for_styles(
        &mut self,
        tree: &Tree,
        font_system: &tiny_font::SharedFontSystem,
        viewport: &crate::coordinates::Viewport,
    ) {
        let text = tree.flatten_to_string();
        let lines: Vec<&str> = text.lines().collect();

        for line_idx in 0..self.line_cache.len() {
            let line_info = &self.line_cache[line_idx];
            let glyph_range = line_info.char_range.clone();

            if glyph_range.is_empty() {
                continue;
            }

            // Check if this line has mixed weight/italic settings
            let mut styles_needed: ahash::HashSet<(u16, bool)> = ahash::HashSet::default();
            for glyph_idx in glyph_range.clone() {
                if let Some(glyph) = self.layout_cache.get(glyph_idx) {
                    // Quantize weight to nearest 100 for cache efficiency
                    let weight_q = ((glyph.weight / 100.0).round() * 100.0) as u16;
                    styles_needed.insert((weight_q, glyph.italic));
                }
            }

            // If only one style, line was already shaped correctly
            if styles_needed.len() <= 1 {
                continue;
            }

            // Line has mixed styles - segment into runs and reshape each
            let line_text = if line_idx < lines.len() {
                lines[line_idx]
            } else {
                continue;
            };

            // Build runs: consecutive glyphs with same (weight, italic)
            #[derive(Debug)]
            struct StyleRun {
                glyph_start: usize, // Index in layout_cache
                glyph_end: usize,   // Exclusive
                char_start: usize,  // Index in line_text
                char_end: usize,    // Exclusive
                weight: f32,
                italic: bool,
            }

            let mut runs: Vec<StyleRun> = Vec::new();
            let mut current_run: Option<StyleRun> = None;

            for (i, glyph_idx) in glyph_range.clone().enumerate() {
                let glyph = &self.layout_cache[glyph_idx];
                let weight_q = ((glyph.weight / 100.0).round() * 100.0) as u16;

                // Check if we need to start a new run
                let need_new_run = if let Some(ref run) = current_run {
                    let run_weight_q = ((run.weight / 100.0).round() * 100.0) as u16;
                    weight_q != run_weight_q || glyph.italic != run.italic
                } else {
                    true
                };

                if need_new_run {
                    // Finish previous run
                    if let Some(run) = current_run.take() {
                        runs.push(run);
                    }

                    // Start new run
                    current_run = Some(StyleRun {
                        glyph_start: glyph_idx,
                        glyph_end: glyph_idx + 1,
                        char_start: i,
                        char_end: i + 1,
                        weight: glyph.weight,
                        italic: glyph.italic,
                    });
                } else {
                    // Extend current run
                    if let Some(ref mut run) = current_run {
                        run.glyph_end = glyph_idx + 1;
                        run.char_end = i + 1;
                    }
                }
            }

            // Don't forget last run
            if let Some(run) = current_run {
                runs.push(run);
            }

            // If we only have one run, nothing to do
            if runs.len() <= 1 {
                continue;
            }

            // Reshape each run with appropriate weight/italic
            let y_pos = line_info.y_position;
            let mut x_offset = 0.0;

            for run in &runs {
                // Extract text for this run from the glyphs themselves (handles tabs correctly)
                let run_text: String = self.layout_cache[run.glyph_start..run.glyph_end]
                    .iter()
                    .map(|g| g.char)
                    .collect();

                // Shape with this run's weight/italic
                let mut shaping_opts = tiny_font::ShapingOptions::default();
                shaping_opts.font_size = viewport.metrics.font_size * viewport.scale_factor;
                shaping_opts.weight = run.weight;
                shaping_opts.italic = run.italic;

                let shaped = font_system.layout_text_shaped_with_tabs(
                    &run_text,
                    viewport.metrics.font_size,
                    viewport.scale_factor,
                    Some(&shaping_opts),
                );

                // Replace glyphs in layout_cache for this run
                let old_glyphs: Vec<_> = self.layout_cache[run.glyph_start..run.glyph_end].to_vec();

                // Verify glyph count matches (should be same after reshaping)
                if shaped.glyphs.len() != old_glyphs.len() {
                    // Glyph count mismatch - skip this line to avoid corruption
                    break;
                }

                // Update glyphs with new shapes but preserve token data AND current decorations
                for (i, shaped_glyph) in shaped.glyphs.iter().enumerate() {
                    let glyph_idx = run.glyph_start + i;
                    let old_glyph = &old_glyphs[i];

                    // Read CURRENT decorations from layout_cache (not old_glyphs!)
                    // apply_demo_styles already set these before reshaping
                    let current_underline = self.layout_cache[glyph_idx].underline;
                    let current_strikethrough = self.layout_cache[glyph_idx].strikethrough;

                    self.layout_cache[glyph_idx] = UnifiedGlyph {
                        char: shaped_glyph.char,
                        layout_pos: LayoutPos::new(
                            x_offset + shaped_glyph.pos.x.0 / viewport.scale_factor,
                            y_pos + shaped_glyph.pos.y.0 / viewport.scale_factor,
                        ),
                        physical_pos: PhysicalPos::new(
                            x_offset * viewport.scale_factor + shaped_glyph.pos.x.0,
                            y_pos * viewport.scale_factor + shaped_glyph.pos.y.0,
                        ),
                        physical_width: shaped_glyph.size.width.0,
                        tex_coords: shaped_glyph.tex_coords,
                        char_byte_offset: old_glyph.char_byte_offset,
                        token_id: old_glyph.token_id,
                        relative_pos: old_glyph.relative_pos,
                        atlas_index: shaped_glyph.atlas_index,
                        weight: run.weight,
                        italic: run.italic,
                        underline: current_underline,
                        strikethrough: current_strikethrough,
                    };
                }

                x_offset += shaped.width / viewport.scale_factor;
            }
        }
    }

    /// Parse and apply demo style tags from text (for theme showcase)
    /// Looks for patterns like [w700], [italic], [underline], [strike], [w700+italic+underline], etc.
    pub fn apply_demo_styles(&mut self, tree: &Tree) {
        let text = tree.flatten_to_string();
        let text = text.as_str();

        // Parse each line for style tags
        for (line_idx, line_text) in text.lines().enumerate() {
            // Find style tag at start of line content (after box drawing chars)
            if let Some(tag_start) = line_text.find('[') {
                if let Some(tag_end) = line_text[tag_start..].find(']') {
                    let tag_end = tag_start + tag_end;
                    let tag = &line_text[tag_start + 1..tag_end];

                    // Parse tag into style attributes
                    let (weight, italic, underline, strikethrough) = parse_demo_tag(tag);

                    // Find the line in line_cache
                    if let Some(line_info) = self.line_cache.get(line_idx) {
                        // Calculate byte offsets for tag boundaries
                        let line_start_byte = line_info.byte_range.start;
                        let tag_start_byte = line_start_byte + tag_start;
                        let tag_end_byte = line_start_byte + tag_end + 1; // +1 to be past the ']'

                        // Apply styles to all glyphs on this line AFTER the tag
                        // Use byte offsets to handle tabs correctly (tabs expand to multiple glyphs)
                        for glyph_idx in line_info.char_range.clone() {
                            if let Some(glyph) = self.layout_cache.get_mut(glyph_idx) {
                                // Skip glyphs before and including the tag
                                if glyph.char_byte_offset < tag_end_byte {
                                    continue;
                                }

                                glyph.weight = weight;
                                glyph.italic = italic;
                                glyph.underline = underline;
                                glyph.strikethrough = strikethrough;
                            }
                        }
                    }
                }
            }
        }
    }

    /// Update style buffer with legacy token ranges
    /// If theme is provided, also apply style attributes (weight, italic, underline, strikethrough)
    pub fn update_syntax(&mut self, tokens: &[TokenRange], fresh_parse: bool) {
        self.update_syntax_with_theme(tokens, None, fresh_parse, None, None, None, None);
    }

    /// Update style buffer with token ranges and optional theme styling
    pub fn update_syntax_with_theme(
        &mut self,
        tokens: &[TokenRange],
        theme: Option<&crate::theme::Theme>,
        fresh_parse: bool,
        tree: Option<&Tree>,
        font_system: Option<&tiny_font::SharedFontSystem>,
        viewport: Option<&crate::coordinates::Viewport>,
        language: Option<&str>,
    ) {
        // Early exit if no tokens - preserve existing highlights (don't clear!)
        // This prevents white text when waiting for fresh parse
        if tokens.is_empty() {
            if fresh_parse {
                // Fresh parse with no tokens means parse failed or doc is empty - clear
                for glyph in &mut self.layout_cache {
                    glyph.token_id = 0;
                    glyph.relative_pos = 0.0;
                    // Reset style attributes to defaults
                    glyph.weight = 400.0;
                    glyph.italic = false;
                    glyph.underline = false;
                    glyph.strikethrough = false;
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
            // Reset style attributes to defaults
            glyph.weight = 400.0;
            glyph.italic = false;
            glyph.underline = false;
            glyph.strikethrough = false;
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

                // Apply style attributes from theme if available
                if let Some(theme) = theme {
                    if let Some(style) =
                        theme.get_token_style_for_language(token.token_id, language)
                    {
                        // Apply weight override (or keep default 400.0)
                        if let Some(weight) = style.weight {
                            self.layout_cache[glyph_idx].weight = weight;
                        }
                        // Apply italic override (or keep default false)
                        if let Some(italic) = style.italic {
                            self.layout_cache[glyph_idx].italic = italic;
                        }
                        // Apply decorations
                        self.layout_cache[glyph_idx].underline = style.underline;
                        self.layout_cache[glyph_idx].strikethrough = style.strikethrough;
                    }
                }

                glyph_idx += 1;
            }
        }

        // If theme has weight/italic overrides, reshape lines with mixed styles
        // Only on fresh parse to avoid flickering during incremental edits
        if fresh_parse {
            if let (Some(tree), Some(font_system), Some(viewport)) = (tree, font_system, viewport) {
                self.reshape_for_styles(tree, font_system, viewport);
            }
        }
    }

    /// Infer style from surrounding context for dirty regions
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

        // Handle empty documents and fallback for edge cases
        if self.line_cache.is_empty() {
            self.visible_lines = 0..0;
        } else if start_line.is_none() && end_line.is_none() {
            // No lines found in visible range - show nothing
            self.visible_lines = 0..0;
        } else {
            self.visible_lines = start_line.unwrap_or(0)..end_line.unwrap_or(0);
        }

        // Find visible characters (includes all chars from visible lines)
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

    // === Cluster-Aware Cursor Positioning ===

    /// Get the cluster map for a specific line
    pub fn get_cluster_map(&self, line: u32) -> Option<&tiny_font::ClusterMap> {
        self.cluster_maps.get(line as usize)
    }

    /// Check if a byte position within a line is at a cluster boundary (valid cursor position)
    /// This is essential for ligatures - cursor can only be placed at cluster boundaries
    pub fn is_valid_cursor_position(&self, line: u32, byte_offset_in_line: usize) -> bool {
        if let Some(cluster_map) = self.get_cluster_map(line) {
            cluster_map.is_cluster_boundary(byte_offset_in_line)
        } else {
            true // If no cluster map, all positions are valid (fallback)
        }
    }

    /// Snap a byte position to the nearest valid cluster boundary
    /// Returns the adjusted byte offset within the line
    pub fn snap_to_cluster_boundary(&self, line: u32, byte_offset_in_line: usize) -> usize {
        if let Some(cluster_map) = self.get_cluster_map(line) {
            cluster_map.snap_to_cluster_boundary(byte_offset_in_line)
        } else {
            byte_offset_in_line // No cluster map, return as-is
        }
    }

    /// Check if a cluster at a given byte position is a ligature
    pub fn is_ligature_at(&self, line: u32, byte_offset_in_line: usize) -> bool {
        if let Some(cluster_map) = self.get_cluster_map(line) {
            // Find which cluster this byte is in
            for cluster_idx in 0..cluster_map.cluster_count() {
                if cluster_map.is_ligature(cluster_idx) {
                    // Check if this byte is in this ligature cluster
                    if let Some((start, end)) = cluster_map.glyph_to_byte_range(cluster_idx) {
                        if byte_offset_in_line >= start && byte_offset_in_line < end {
                            return true;
                        }
                    }
                }
            }
        }
        false
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

    // === Style Helper Methods ===

    /// Get style attributes from a glyph as a tuple (weight, italic, underline, strikethrough)
    pub fn get_glyph_style(&self, glyph: &UnifiedGlyph) -> (f32, bool, bool, bool) {
        (
            glyph.weight,
            glyph.italic,
            glyph.underline,
            glyph.strikethrough,
        )
    }

    /// Get style attributes for a specific glyph index
    pub fn get_glyph_style_at(&self, glyph_idx: usize) -> Option<(f32, bool, bool, bool)> {
        self.layout_cache
            .get(glyph_idx)
            .map(|g| self.get_glyph_style(g))
    }

    // === Position Lookup Methods ===

    /// Get the precise layout position (in logical pixels) for a byte offset
    /// Returns None if the byte offset is out of bounds
    pub fn get_position_at_byte(&self, byte_offset: usize) -> Option<LayoutPos> {
        // Binary search for the glyph at or before this byte offset
        match self
            .layout_cache
            .binary_search_by_key(&byte_offset, |g| g.char_byte_offset)
        {
            Ok(idx) => self.layout_cache.get(idx).map(|g| g.layout_pos),
            Err(idx) => {
                // Not exact match - get the previous glyph if it exists
                if idx > 0 {
                    self.layout_cache.get(idx - 1).map(|g| g.layout_pos)
                } else {
                    None
                }
            }
        }
    }

    /// Get precise position at line/column (character index within line, not visual column)
    /// Returns None if line or column is out of bounds
    pub fn get_position_at_line_col(&self, line: u32, col: usize) -> Option<LayoutPos> {
        let line_info = self.line_cache.get(line as usize)?;
        let glyph_idx = line_info.char_range.start + col;
        self.layout_cache.get(glyph_idx).map(|g| g.layout_pos)
    }

    /// Get precise X position for a column within a line
    /// More efficient than get_position_at_line_col when you only need X
    pub fn get_x_at_line_col(&self, line: u32, col: usize) -> Option<f32> {
        self.get_position_at_line_col(line, col).map(|pos| pos.x.0)
    }
}

/// Convert token type to ID for palette lookup
pub fn token_type_to_id(token: crate::syntax::TokenType) -> u16 {
    // Use the centralized function from syntax.rs
    crate::syntax::SyntaxHighlighter::token_type_to_id(token) as u16
}
