//! Tab Manager - manages multiple open files

use crate::coordinates::Viewport;
use crate::diagnostics_manager::DiagnosticsManager;
use crate::line_numbers_plugin::LineNumbersPlugin;
use crate::scroll::Scrollable;
use crate::text_editor_plugin::TextEditorPlugin;
use crate::text_renderer::TextRenderer;
use std::path::PathBuf;
use tiny_core::tree::{Point, Rect};
use tiny_sdk::LogicalPixels;

pub struct Tab {
    pub plugin: TextEditorPlugin,
    pub line_numbers: LineNumbersPlugin,
    pub diagnostics: DiagnosticsManager,
    pub text_renderer: TextRenderer,
    pub display_name: String,
    pub scroll_position: Point,
    /// The ACTUAL syntax highlighter Arc (shared with InputHandler)
    /// This is the source of truth for parse results
    pub syntax_arc: Option<std::sync::Arc<crate::syntax::SyntaxHighlighter>>,
}

impl Tab {
    pub fn new(plugin: TextEditorPlugin) -> Self {
        let display_name = if let Some(ref path) = plugin.file_path {
            path.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("Untitled")
                .to_string()
        } else {
            "Untitled".to_string()
        };

        // Create text renderer first (needed for precise diagnostic positions)
        let text_renderer = TextRenderer::new();

        // Open file in diagnostics manager if we have a path
        let mut diagnostics = DiagnosticsManager::new();
        if let Some(ref path) = plugin.file_path {
            let content = plugin.editor.view.doc.read().flatten_to_string();
            diagnostics.open_file(path.clone(), (*content).clone(), &text_renderer);
        }

        // Get syntax Arc from InputHandler - this is the master that receives parse results
        let syntax_arc = plugin.editor.input.syntax_highlighter();

        if let Some(ref arc) = syntax_arc {
            eprintln!("🆕 NEW TAB: Created with syntax_arc ptr={:p} for {}",
                arc.as_ref() as *const _, arc.language());
        }

        Self {
            plugin,
            line_numbers: LineNumbersPlugin::new(),
            diagnostics,
            text_renderer,
            display_name,
            scroll_position: Point::default(),
            syntax_arc,
        }
    }

    pub fn from_file(path: PathBuf) -> Result<Self, std::io::Error> {
        Self::from_file_with_event_emitter(path, None::<fn(&str)>)
    }

    pub fn from_file_with_event_emitter<F>(path: PathBuf, emit_event: Option<F>) -> Result<Self, std::io::Error>
    where
        F: Fn(&str) + Send + Sync + 'static,
    {
        // Canonicalize path for consistent comparison across tabs
        let canonical_path = std::fs::canonicalize(&path).unwrap_or(path);
        let plugin = TextEditorPlugin::from_file_with_event_emitter(canonical_path, emit_event)?;
        Ok(Self::new(plugin))
    }

    pub fn is_modified(&self) -> bool {
        self.plugin.is_modified()
    }

    pub fn path(&self) -> Option<&PathBuf> {
        self.plugin.file_path.as_ref()
    }

    /// Collect all paintable plugins from this tab (generic - works with any plugin tree)
    pub fn collect_paint_ops<F>(
        &self,
        paint_ops: &mut Vec<(i32, Box<dyn FnOnce(&mut wgpu::RenderPass)>)>,
        editor_bounds: tiny_sdk::types::LayoutRect,
        viewport_scroll: tiny_sdk::LayoutPos,
        make_ctx: F,
    )
    where
        F: Fn(tiny_sdk::types::WidgetViewport) -> Option<tiny_sdk::PaintContext> + Copy,
    {
        // Editor view's plugins (cursor, selection, etc.)
        self.plugin.editor.collect_paint_ops(paint_ops, editor_bounds, make_ctx);

        // Note: Diagnostics plugin is rendered separately (see paint_diagnostics method)
        // due to different ownership model (owned by Tab not EditableTextView)
    }

    /// Paint diagnostics plugin directly - must be called separately from collect_paint_ops
    /// due to Rust lifetime constraints when borrowing through tab_manager
    pub fn paint_diagnostics<F>(
        &self,
        pass: &mut wgpu::RenderPass,
        editor_bounds: tiny_sdk::types::LayoutRect,
        viewport_scroll: tiny_sdk::LayoutPos,
        make_ctx: F,
        font_system: &std::sync::Arc<tiny_font::SharedFontSystem>,
    ) where
        F: Fn(tiny_sdk::types::WidgetViewport) -> Option<tiny_sdk::PaintContext>,
    {
        if let Some(ref plugin_arc) = self.diagnostics.plugin() {
            if let Ok(mut plugin) = plugin_arc.lock() {
                let viewport = tiny_sdk::types::WidgetViewport {
                    bounds: editor_bounds,
                    scroll: viewport_scroll,
                    content_margin: tiny_sdk::types::LayoutPos::new(0.0, 0.0),
                    widget_id: 3,
                };

                if let Some(ctx) = make_ctx(viewport) {
                    // Set viewport info and font system before painting
                    if let Some(library) = plugin.as_library_mut() {
                        let viewport_bytes = bytemuck::bytes_of(&ctx.viewport);
                        let _ = library.call("set_viewport_info", viewport_bytes);

                        // Set font system pointer
                        let font_ptr = font_system as *const _ as u64;
                        let _ = library.call("set_font_system", &font_ptr.to_le_bytes());
                    }

                    if let Some(paintable) = plugin.as_paintable() {
                        paintable.paint(&ctx, pass);
                    }
                }
            }
        }
        // Plugin not initialized - silently skip (expected during startup)
    }
}

pub struct TabManager {
    tabs: Vec<Tab>,
    active_index: usize,
}

impl TabManager {
    pub fn new() -> Self {
        Self {
            tabs: Vec::new(),
            active_index: 0,
        }
    }

    /// Create with an initial tab
    pub fn with_initial_tab(tab: Tab) -> Self {
        Self {
            tabs: vec![tab],
            active_index: 0,
        }
    }

    /// Get the active tab
    pub fn active_tab(&self) -> Option<&Tab> {
        self.tabs.get(self.active_index)
    }

    /// Get the active tab mutably
    pub fn active_tab_mut(&mut self) -> &mut Tab {
        &mut self.tabs[self.active_index]
    }

    /// Get all tabs
    pub fn tabs(&self) -> &[Tab] {
        &self.tabs
    }

    pub fn tabs_mut(&mut self) -> &mut [Tab] {
        &mut self.tabs
    }

    /// Get active index
    pub fn active_index(&self) -> usize {
        self.active_index
    }

    /// Get a specific tab mutably by index
    pub fn tab_mut(&mut self, index: usize) -> Option<&mut Tab> {
        self.tabs.get_mut(index)
    }

    /// Add a new tab and make it active
    pub fn add_tab(&mut self, tab: Tab) {
        self.tabs.push(tab);
        self.active_index = self.tabs.len() - 1;
    }

    /// Switch to a tab by index
    /// Returns true if the tab actually changed
    pub fn switch_to(&mut self, index: usize) -> bool {
        if index < self.tabs.len() && index != self.active_index {
            self.active_index = index;

            // Notify LSP about the file switch
            let tab = &mut self.tabs[index];
            if let Some(ref path) = tab.plugin.file_path {
                let content = tab.plugin.editor.view.doc.read().flatten_to_string();
                tab.diagnostics
                    .lsp_service_mut()
                    .open_file(path.clone(), (*content).clone());
            }

            true
        } else {
            false
        }
    }

    /// Close a tab by index
    /// Returns true if we closed the last tab (app should exit or open new tab)
    pub fn close_tab(&mut self, index: usize) -> bool {
        if index >= self.tabs.len() {
            return false;
        }

        self.tabs.remove(index);

        if self.tabs.is_empty() {
            return true;
        }

        // Adjust active index if needed
        if self.active_index >= self.tabs.len() {
            self.active_index = self.tabs.len() - 1;
        } else if index < self.active_index {
            self.active_index -= 1;
        }

        false
    }

    /// Close the active tab
    pub fn close_active_tab(&mut self) -> bool {
        self.close_tab(self.active_index)
    }

    /// Check if a file is already open
    pub fn find_tab_by_path(&self, path: &PathBuf) -> Option<usize> {
        self.tabs.iter().position(|tab| tab.path() == Some(path))
    }

    /// Open a file with event emitter for instant syntax highlighting
    pub fn open_file_with_event_emitter<F>(&mut self, path: PathBuf, emit_event: Option<F>) -> Result<bool, std::io::Error>
    where
        F: Fn(&str) + Send + Sync + 'static,
    {
        // Canonicalize path for consistent comparison
        let canonical_path = std::fs::canonicalize(&path).unwrap_or(path);

        // Check if already open
        if let Some(index) = self.find_tab_by_path(&canonical_path) {
            let switched = self.switch_to(index);
            return Ok(switched);
        }

        // Open new tab with event emitter
        let tab = Tab::from_file_with_event_emitter(canonical_path, emit_event)?;
        self.add_tab(tab);
        Ok(true)
    }

    /// Open a file (or switch to it if already open)
    /// Returns Ok(true) if a tab switch/open occurred, Ok(false) if no change
    pub fn open_file(&mut self, path: PathBuf) -> Result<bool, std::io::Error> {
        self.open_file_with_event_emitter(path, None::<fn(&str)>)
    }

    /// Get the number of tabs
    pub fn len(&self) -> usize {
        self.tabs.len()
    }

    /// Check if there are no tabs
    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }
}

// === Scrollable Implementation for Tab ===

impl Scrollable for Tab {
    fn get_scroll(&self) -> Point {
        self.scroll_position
    }

    fn set_scroll(&mut self, scroll: Point) {
        self.scroll_position = scroll;
    }

    fn handle_scroll(&mut self, delta: Point, viewport: &Viewport, widget_bounds: Rect) -> bool {
        // Apply scroll delta (inverted for natural scrolling)
        self.scroll_position.y.0 -= delta.y.0;
        self.scroll_position.x.0 -= delta.x.0;

        // Get document for proper clamping
        let doc = &self.plugin.editor.view.doc;
        let tree = doc.read();

        // Use viewport's proper clamp_scroll_to_bounds with actual widget bounds
        // This handles all edge cases, soft wrap, etc.
        let mut temp_viewport = viewport.clone();
        temp_viewport.scroll = self.scroll_position;
        temp_viewport.clamp_scroll_to_bounds(&tree, widget_bounds);
        self.scroll_position = temp_viewport.scroll;

        true // Always handle scroll for editor
    }

    fn get_content_bounds(&self, viewport: &Viewport) -> Rect {
        // Content bounds based on actual document size and viewport metrics
        let doc = &self.plugin.editor.view.doc;
        let tree = doc.read();
        let line_count = tree.line_count();

        // Use actual line height from viewport metrics
        let content_height = (line_count as f32) * viewport.metrics.line_height;

        // Calculate maximum line width from document
        let mut max_width = 0.0f32;
        for line_idx in 0..line_count {
            let line_text = tree.line_text(line_idx);
            let line_width = (line_text.len() as f32) * viewport.metrics.space_width;
            max_width = max_width.max(line_width);
        }

        Rect {
            x: LogicalPixels(0.0),
            y: LogicalPixels(0.0),
            width: LogicalPixels(max_width),
            height: LogicalPixels(content_height),
        }
    }
}
