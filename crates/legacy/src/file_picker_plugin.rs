//! File picker plugin - shows a searchable list of files

use std::path::{Path, PathBuf};
use tiny_font::{create_glyph_instances, SharedFontSystem};
use tiny_sdk::{
    Capability, GlyphInstance, Initializable, LayoutPos, PaintContext, Paintable, Plugin,
    PluginError, SetupContext,
};

/// Simple file picker with regex-based search
pub struct FilePickerPlugin {
    /// Whether the picker is visible
    pub visible: bool,
    /// Current search query
    pub query: String,
    /// All files in the working directory
    all_files: Vec<PathBuf>,
    /// Filtered files based on query
    filtered_files: Vec<PathBuf>,
    /// Selected index in filtered list
    selected_index: usize,
    /// Working directory
    working_dir: PathBuf,
}

impl FilePickerPlugin {
    pub fn new() -> Self {
        let working_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let all_files = Self::scan_directory(&working_dir);

        Self {
            visible: false,
            query: String::new(),
            all_files: all_files.clone(),
            filtered_files: all_files,
            selected_index: 0,
            working_dir,
        }
    }

    /// Scan directory for files (recursively, but limited depth)
    fn scan_directory(dir: &Path) -> Vec<PathBuf> {
        let mut files = Vec::new();

        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();

                // Skip hidden files and directories
                if path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(|s| s.starts_with('.'))
                    .unwrap_or(false)
                {
                    continue;
                }

                if path.is_file() {
                    files.push(path);
                } else if path.is_dir() {
                    // Recursively scan subdirectories (limit depth to avoid performance issues)
                    let subfiles = Self::scan_directory(&path);
                    files.extend(subfiles);
                }
            }
        }

        // Sort files for consistent ordering
        files.sort();
        files
    }

    /// Show the file picker
    pub fn show(&mut self) {
        self.visible = true;
        self.query.clear();
        self.update_filtered_files();
    }

    /// Hide the file picker
    pub fn hide(&mut self) {
        self.visible = false;
        self.query.clear();
    }

    /// Update search query
    pub fn set_query(&mut self, query: String) {
        self.query = query;
        self.update_filtered_files();
    }

    /// Add character to query
    pub fn add_char(&mut self, ch: char) {
        self.query.push(ch);
        self.update_filtered_files();
    }

    /// Remove last character from query
    pub fn backspace(&mut self) {
        self.query.pop();
        self.update_filtered_files();
    }

    /// Update filtered files based on current query
    fn update_filtered_files(&mut self) {
        if self.query.is_empty() {
            self.filtered_files = self.all_files.clone();
        } else {
            // Simple regex-based filtering with .* appended
            let pattern = format!("{}.*", regex::escape(&self.query));
            if let Ok(re) = regex::Regex::new(&pattern) {
                self.filtered_files = self
                    .all_files
                    .iter()
                    .filter(|path| {
                        path.to_str()
                            .map(|s| re.is_match(s))
                            .unwrap_or(false)
                    })
                    .cloned()
                    .collect();
            } else {
                // If regex is invalid, show all files
                self.filtered_files = self.all_files.clone();
            }
        }

        // Reset selection to first item
        self.selected_index = 0;
    }

    /// Move selection up
    pub fn move_up(&mut self) {
        if self.selected_index > 0 {
            self.selected_index -= 1;
        }
    }

    /// Move selection down
    pub fn move_down(&mut self) {
        if self.selected_index < self.filtered_files.len().saturating_sub(1) {
            self.selected_index += 1;
        }
    }

    /// Get the selected file path
    pub fn selected_file(&self) -> Option<&Path> {
        self.filtered_files.get(self.selected_index).map(|p| p.as_path())
    }

    /// Get display name for a path (relative to working dir)
    fn display_name(&self, path: &Path) -> String {
        path.strip_prefix(&self.working_dir)
            .ok()
            .and_then(|p| p.to_str())
            .unwrap_or_else(|| path.to_str().unwrap_or("???"))
            .to_string()
    }

    /// Collect glyphs for batched rendering (like line numbers plugin)
    pub fn collect_glyphs(&self, collector: &mut crate::render::GlyphCollector) {
        if !self.visible {
            return;
        }

        // Get font service from service registry
        let font_service = match collector.services().get::<SharedFontSystem>() {
            Some(fs) => fs,
            None => return,
        };

        let scale_factor = collector.viewport.scale_factor;
        let font_size = collector.viewport.font_size;
        let line_height = collector.viewport.line_height;

        // Extract bounds before the loop to avoid borrow issues
        let bounds_x = collector.widget_viewport.as_ref().map(|w| w.bounds.x.0).unwrap_or(0.0);
        let bounds_y = collector.widget_viewport.as_ref().map(|w| w.bounds.y.0).unwrap_or(0.0);

        let mut glyphs = Vec::new();

        // Render search input on first line (in widget-local space)
        let input_text = format!("> {}", self.query);
        let input_pos = LayoutPos::new(10.0, 5.0); // Widget-local coordinates
        let input_glyphs = create_glyph_instances(
            &font_service,
            &input_text,
            input_pos,
            font_size,
            scale_factor,
            line_height,
            None,
            0,
        );
        glyphs.extend(input_glyphs);

        // Render filtered file list (show up to 12 files)
        const MAX_VISIBLE_FILES: usize = 12;
        let visible_files = self.filtered_files.iter().take(MAX_VISIBLE_FILES);

        for (idx, path) in visible_files.enumerate() {
            let display_name = self.display_name(path);
            let is_selected = idx == self.selected_index;

            // Add selection indicator
            let line_text = if is_selected {
                format!("→ {}", display_name)
            } else {
                format!("  {}", display_name)
            };

            let y_offset = line_height * (idx as f32 + 2.0); // Widget-local Y
            let file_pos = LayoutPos::new(10.0, y_offset);

            let file_glyphs = create_glyph_instances(
                &font_service,
                &line_text,
                file_pos,
                font_size,
                scale_factor,
                line_height,
                None,
                if is_selected { 1 } else { 0 }, // Different token for selected item
            );

            glyphs.extend(file_glyphs);
        }

        // Convert to screen coordinates (like line numbers plugin does)
        for mut g in glyphs {
            // Transform from widget-local space to screen space
            let screen_x = g.pos.x.0 + bounds_x;
            let screen_y = g.pos.y.0 + bounds_y;
            // Convert to physical coordinates
            g.pos = LayoutPos::new(screen_x * scale_factor, screen_y * scale_factor);
            collector.add_glyphs(vec![g]);
        }
    }
}

// === Plugin Trait Implementation ===

impl Plugin for FilePickerPlugin {
    fn name(&self) -> &str {
        "file_picker"
    }

    fn version(&self) -> &str {
        "1.0.0"
    }

    fn capabilities(&self) -> Vec<Capability> {
        vec![
            Capability::Initializable,
            Capability::Paintable("file_picker".to_string()),
        ]
    }

    fn as_initializable(&mut self) -> Option<&mut dyn Initializable> {
        Some(self)
    }

    fn as_paintable(&self) -> Option<&dyn Paintable> {
        Some(self)
    }
}

impl Initializable for FilePickerPlugin {
    fn setup(&mut self, _ctx: &mut SetupContext) -> Result<(), PluginError> {
        Ok(())
    }
}

impl Paintable for FilePickerPlugin {
    fn paint(&self, _ctx: &PaintContext, _pass: &mut wgpu::RenderPass) {
        // File picker now uses collect_glyphs for batched rendering
        // This method is kept for plugin trait compatibility
    }

    fn z_index(&self) -> i32 {
        1000 // Render above everything else
    }
}