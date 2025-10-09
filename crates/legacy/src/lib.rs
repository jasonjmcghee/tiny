extern crate lazy_static;

// #[global_allocator]
// static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

// Host-only GPU FFI implementations
pub mod gpu_ffi_host;

pub mod accelerator;
#[cfg(feature = "winit")]
pub mod app;
pub mod config;
pub mod diagnostics_manager;
pub mod editor_logic;
pub mod event_data;
pub mod file_picker_plugin;
pub mod filterable_dropdown;
pub mod overlay_picker;
pub mod grep_plugin;
pub mod history;
pub mod input;
pub mod input_types;
pub mod io;
pub mod line_numbers_plugin;
pub mod lsp_manager;
pub mod lsp_service;
pub use diagnostics_plugin;
pub mod editable_text_view;
pub mod render;
pub mod scrollbar_plugin;
pub mod shortcuts;
pub mod tab_bar_plugin;
pub mod tab_manager;
pub mod text_editor_plugin;

// Import UI components from tiny-ui
pub use tiny_ui::{
    coordinates, scroll, syntax, text_effects, text_renderer, text_view, theme, widget, Scrollable,
    SyntaxHighlighter, TextRenderer, TextView, Theme, Viewport, Widget,
};
#[cfg(feature = "winit")]
pub mod winit_adapter;

// Re-export core types
pub use history::History;
pub use input::{InputHandler, Selection};
pub use render::Renderer;
pub use tiny_tree::{Content, Doc, Edit, Point, Rect, Span, Tree as DocTree};
