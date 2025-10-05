//! File picker plugin - searchable file list

use crate::{overlay_picker::OverlayPicker, scroll::Scrollable, Widget};
use crate::coordinates::Viewport;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tiny_core::tree::{Point, Rect};
use tiny_font::SharedFontSystem;
use tiny_sdk::{Capability, Initializable, PaintContext, Paintable, Plugin, PluginError, SetupContext};

/// File picker plugin for finding and opening files
pub struct FilePickerPlugin {
    picker: OverlayPicker<PathBuf>,
    working_dir: PathBuf,
    pub visible: bool,
}

impl FilePickerPlugin {
    pub fn new() -> Self {
        let working_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let wd_clone = working_dir.clone();

        // Format function
        let format_fn = move |path: &PathBuf| {
            path.strip_prefix(&wd_clone)
                .ok()
                .and_then(|p| p.to_str())
                .unwrap_or_else(|| path.to_str().unwrap_or("???"))
                .to_string()
        };

        // Search function (substring filter with scoring)
        let search_fn = |query: &str, items: &[PathBuf]| {
            let query_lower = query.to_lowercase();
            let mut results: Vec<(PathBuf, u32)> = items.iter()
                .filter_map(|path| {
                    path.to_str().and_then(|s| {
                        let s_lower = s.to_lowercase();
                        if s_lower.contains(&query_lower) {
                            Some((path.clone(), (1000 - s_lower.find(&query_lower).unwrap_or(999)) as u32))
                        } else {
                            None
                        }
                    })
                })
                .collect();
            results.sort_by(|a, b| b.1.cmp(&a.1));
            results.into_iter().map(|(p, _)| p).collect()
        };

        let mut picker = OverlayPicker::new(format_fn, search_fn);

        // Spawn background thread to scan directory
        let cached_items = picker.cached_items.clone();
        let wd = working_dir.clone();
        std::thread::spawn(move || {
            *cached_items.write() = Self::scan_directory(&wd);
        });

        Self { picker, working_dir, visible: false }
    }

    /// Scan directory for files
    fn scan_directory(dir: &Path) -> Vec<PathBuf> {
        use ignore::WalkBuilder;
        let mut files: Vec<PathBuf> = WalkBuilder::new(dir)
            .hidden(true).git_ignore(true).git_exclude(true)
            .build()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().map(|ft| ft.is_file()).unwrap_or(false))
            .map(|e| e.into_path())
            .collect();
        files.sort();
        files
    }

    pub fn show(&mut self) {
        self.visible = true;
        let files = self.picker.cached_items.read().clone();
        self.picker.show_with_title(files, "Open File");
    }

    pub fn hide(&mut self) {
        self.visible = false;
        self.picker.hide();
    }

    pub fn add_char(&mut self, ch: char) { self.picker.add_char(ch); }
    pub fn backspace(&mut self) { self.picker.backspace(); }
    pub fn move_up(&mut self) { self.picker.move_up(); }
    pub fn move_down(&mut self) { self.picker.move_down(); }
    pub fn selected_file(&self) -> Option<&Path> { self.picker.selected_item().map(|p| p.as_path()) }
    pub fn set_query(&mut self, query: String) {
        self.picker.dropdown.input.set_text(&query);
        self.picker.trigger_filter(query);
    }
}

tiny_sdk::plugin! {
    FilePickerPlugin {
        name: "file_picker",
        version: "1.0.0",
        z_index: 1000,
        traits: [Init, Paint],
        defaults: [Init, Paint],
    }
}

impl Scrollable for FilePickerPlugin {
    fn get_scroll(&self) -> Point { self.picker.get_scroll() }
    fn set_scroll(&mut self, scroll: Point) { self.picker.set_scroll(scroll); }
    fn handle_scroll(&mut self, delta: Point, viewport: &Viewport, widget_bounds: Rect) -> bool {
        self.picker.handle_scroll(delta, viewport, widget_bounds)
    }
    fn get_content_bounds(&self, viewport: &Viewport) -> Rect { self.picker.get_content_bounds(viewport) }
}

tiny_ui::impl_widget_delegate!(FilePickerPlugin, picker);
