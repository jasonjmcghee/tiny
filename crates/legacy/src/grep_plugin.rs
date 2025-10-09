//! Grep plugin - full codebase search

use crate::coordinates::Viewport;
use crate::input::{Event, EventSubscriber, PropagationControl};
use crate::{overlay_picker::OverlayPicker, scroll::Scrollable, Widget};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tiny_core::tree::{Point, Rect};
use tiny_sdk::Plugin;

/// A single grep result
#[derive(Clone, Debug)]
pub struct GrepResult {
    pub file_path: PathBuf,
    pub line_number: usize,
    pub column: usize,
    pub line_content: String,
}

/// Grep plugin for full codebase search
pub struct GrepPlugin {
    pub picker: OverlayPicker<GrepResult>,
    working_dir: PathBuf,
    searching: bool,
    pub visible: bool,
    // Channel for receiving search results from background thread (Mutex for Sync)
    result_rx: Arc<Mutex<std::sync::mpsc::Receiver<Vec<GrepResult>>>>,
    result_tx: std::sync::mpsc::Sender<Vec<GrepResult>>,
    // Redraw notifier to trigger UI updates when results arrive
    redraw_notifier: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl GrepPlugin {
    /// Get the input field for cursor/selection routing
    pub fn input(&self) -> &crate::editable_text_view::EditableTextView {
        &self.picker.dropdown.input
    }

    /// Get mutable input field
    pub fn input_mut(&mut self) -> &mut crate::editable_text_view::EditableTextView {
        &mut self.picker.dropdown.input
    }

    pub fn new() -> Self {
        let working_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

        // Format function
        let format_fn = |result: &GrepResult| {
            let name = result
                .file_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("???");
            format!("{}:{}  {}", name, result.line_number, result.line_content)
        };

        // Search function (empty - results come from background thread)
        let search_fn = |_query: &str, items: &[GrepResult]| items.to_vec();

        // Create channel for background thread communication
        let (result_tx, result_rx) = std::sync::mpsc::channel();

        Self {
            picker: OverlayPicker::new(format_fn, search_fn),
            working_dir,
            searching: false,
            visible: false,
            result_rx: Arc::new(Mutex::new(result_rx)),
            result_tx,
            redraw_notifier: None,
        }
    }

    /// Set the redraw notifier (called when results arrive from background thread)
    pub fn set_redraw_notifier(&mut self, notifier: Arc<dyn Fn() + Send + Sync>) {
        self.redraw_notifier = Some(notifier);
    }

    pub fn show(&mut self, search_term: String) {
        self.visible = true;
        self.searching = !search_term.is_empty();

        if !search_term.is_empty() {
            let tx = self.result_tx.clone();
            let wd = self.working_dir.clone();
            let q = search_term.clone();
            std::thread::spawn(move || {
                let results = Self::search_codebase(&wd, &q);
                let _ = tx.send(results);
            });
        }

        self.picker.show_with_title(Vec::new(), "Search in Files");
        if !search_term.is_empty() {
            self.picker.dropdown.input.set_text(&search_term);
        }
    }

    pub fn hide(&mut self) {
        self.visible = false;
        self.searching = false;
        self.picker.hide();
    }

    fn trigger_search(&mut self, query: String) {
        if query.is_empty() {
            self.searching = false;
            self.picker.dropdown.set_items(Vec::new());
            return;
        }

        // Don't spawn new search if one is already running (prevents thread spam on fast typing)
        if self.searching {
            return;
        }

        self.searching = true;
        let tx = self.result_tx.clone();
        let wd = self.working_dir.clone();
        std::thread::spawn(move || {
            let results = Self::search_codebase(&wd, &query);
            let _ = tx.send(results); // Non-blocking send
        });

        // Results will be polled and displayed when ready
    }

    /// Search codebase using ripgrep via ignore crate
    fn search_codebase(dir: &Path, search_term: &str) -> Vec<GrepResult> {
        use ignore::WalkBuilder;
        use std::io::BufRead;

        let mut results = Vec::new();
        let search_lower = search_term.to_lowercase();

        for entry in WalkBuilder::new(dir)
            .hidden(true)
            .git_ignore(true)
            .git_exclude(true)
            .build()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().map(|ft| ft.is_file()).unwrap_or(false))
        {
            let path = entry.path();

            // Skip binary files (heuristic: check extension)
            if let Some(ext) = path.extension() {
                let ext_str = ext.to_str().unwrap_or("");
                if matches!(
                    ext_str,
                    "png" | "jpg" | "jpeg" | "gif" | "ico" | "pdf" | "zip" | "tar" | "gz"
                ) {
                    continue;
                }
            }

            // Read file and search line by line
            if let Ok(file) = std::fs::File::open(path) {
                let reader = std::io::BufReader::new(file);
                for (line_idx, line) in reader.lines().enumerate() {
                    if let Ok(line_content) = line {
                        // Case-insensitive search
                        if let Some(col) = line_content.to_lowercase().find(&search_lower) {
                            results.push(GrepResult {
                                file_path: path.to_path_buf(),
                                line_number: line_idx + 1, // 1-indexed
                                column: col,
                                line_content: line_content.trim().to_string(),
                            });
                        }
                    }
                }
            }

            // Limit results to prevent memory issues and blocking
            if results.len() >= 200 {
                break;
            }
        }

        results
    }

    pub fn poll_results(&mut self) -> bool {
        if self.searching {
            // Non-blocking receive from background thread
            if let Ok(rx) = self.result_rx.lock() {
                if let Ok(results) = rx.try_recv() {
                    self.picker.dropdown.set_items(results);
                    self.searching = false;

                    // Trigger redraw to show results immediately
                    if let Some(ref notifier) = self.redraw_notifier {
                        notifier();
                    }

                    return true; // Results were received, need redraw
                }
            }
        }
        false
    }

    pub fn move_up(&mut self) {
        self.picker.move_up();
    }
    pub fn move_down(&mut self) {
        self.picker.move_down();
    }
    pub fn selected_result(&self) -> Option<&GrepResult> {
        self.picker.selected_item()
    }

    /// Set the query and trigger search
    /// Note: Assumes input text is already set (by InputHandler)
    pub fn set_query(&mut self, query: String) {
        self.trigger_search(query);
    }
}

impl EventSubscriber for GrepPlugin {
    fn handle_event(
        &mut self,
        event: &Event,
        event_bus: &mut crate::input::EventBus,
    ) -> PropagationControl {
        if !self.visible {
            return PropagationControl::Continue; // Not active, pass through
        }

        use serde_json::json;

        // Handle events: execute logic AND stop propagation
        match event.name.as_str() {
            // Handle Enter key specially - emit action.submit instead of inserting newline
            "editor.insert_newline" => {
                // Single-line input - Enter should submit, not insert newline
                event_bus.emit("action.submit", json!({}), 10, "grep");
                PropagationControl::Stop
            }
            // Handle text editing events internally
            event_name if event_name.starts_with("editor.") => {
                let input = self.input_mut();
                let text_before = input.view.text();

                // Let InputHandler handle the event
                let _action =
                    input
                        .input
                        .handle_event(event, &input.view.doc, &input.view.viewport);

                // Check if text changed, trigger search if so
                let text_after = input.view.text();
                if text_before != text_after {
                    let query = text_after.to_string();
                    self.set_query(query);
                    event_bus.emit("ui.redraw", json!({}), 20, "grep");
                }

                PropagationControl::Stop
            }
            "navigate.up" => {
                self.move_up();
                event_bus.emit("ui.redraw", json!({}), 20, "grep");
                PropagationControl::Stop
            }
            "navigate.down" => {
                self.move_down();
                event_bus.emit("ui.redraw", json!({}), 20, "grep");
                PropagationControl::Stop
            }
            "action.cancel" => {
                self.hide();
                event_bus.emit("overlay.closed", json!({"source": "grep"}), 10, "grep");
                PropagationControl::Stop
            }
            "action.submit" => {
                if let Some(result) = self.selected_result().cloned() {
                    self.hide();
                    event_bus.emit(
                        "file.goto",
                        json!({
                            "file": result.file_path,
                            "line": result.line_number.saturating_sub(1),
                            "column": result.column
                        }),
                        10,
                        "grep",
                    );
                }
                PropagationControl::Stop
            }
            "app.mouse.scroll" => {
                // Handle mouse wheel scrolling
                let delta_y = event
                    .data
                    .get("delta_y")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0) as f32;

                self.picker.dropdown.handle_scroll(delta_y);
                event_bus.emit("ui.redraw", json!({}), 20, "grep");
                PropagationControl::Stop
            }
            "app.mouse.move" => {
                // Handle mouse hover to highlight items
                let x = event.data.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
                let y = event.data.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;

                // Check if mouse is over picker bounds
                let bounds = self.picker.get_bounds();
                let is_over_picker = x >= bounds.x.0
                    && x < bounds.x.0 + bounds.width.0
                    && y >= bounds.y.0
                    && y < bounds.y.0 + bounds.height.0;

                if is_over_picker {
                    if self.picker.handle_hover(x, y) {
                        event_bus.emit("ui.redraw", json!({}), 20, "grep");
                    }
                    PropagationControl::Stop
                } else {
                    PropagationControl::Continue
                }
            }
            _ => PropagationControl::Continue,
        }
    }

    fn priority(&self) -> i32 {
        100 // High priority (overlays filter events before main editor)
    }

    fn is_active(&self) -> bool {
        self.visible
    }
}

impl tiny_sdk::Updatable for GrepPlugin {
    fn update(
        &mut self,
        _dt: f32,
        _ctx: &mut tiny_sdk::UpdateContext,
    ) -> Result<(), tiny_sdk::PluginError> {
        // Poll background search results and mark if we need a redraw
        // The redraw will be triggered by the render loop detecting UI changes
        if self.searching {
            self.poll_results();
        }
        Ok(())
    }
}

tiny_sdk::plugin! {
    GrepPlugin {
        name: "grep",
        version: "1.0.0",
        z_index: 1000,
        traits: [Init, Update, Paint],
        defaults: [Init, Paint],  // Update has custom impl above
    }
}

impl Scrollable for GrepPlugin {
    fn get_scroll(&self) -> Point {
        self.picker.get_scroll()
    }
    fn set_scroll(&mut self, scroll: Point) {
        self.picker.set_scroll(scroll);
    }
    fn handle_scroll(&mut self, delta: Point, viewport: &Viewport, widget_bounds: Rect) -> bool {
        self.picker.handle_scroll(delta, viewport, widget_bounds)
    }
    fn get_content_bounds(&self, viewport: &Viewport) -> Rect {
        self.picker.get_content_bounds(viewport)
    }
}

tiny_ui::impl_widget_delegate!(GrepPlugin, picker);
