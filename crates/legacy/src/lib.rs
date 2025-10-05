extern crate lazy_static;

// #[global_allocator]
// static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

// Host-only GPU FFI implementations
pub mod gpu_ffi_host;

pub mod accelerator;
#[cfg(feature = "winit")]
pub mod app;
pub mod config;
pub mod coordinates; // Coordinate system abstraction
pub mod diagnostics_manager;
pub mod editor_logic;
pub mod file_picker_plugin;
pub mod filterable_dropdown;
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
pub mod gpu_buffer_manager;
pub mod render;
pub mod scroll;
pub mod shortcuts;
pub mod syntax;
pub mod tab_bar_plugin;
pub mod tab_manager;
pub mod text_editor_plugin;
pub mod text_effects;
pub mod text_renderer;
pub mod text_view;
pub mod theme;
#[cfg(feature = "winit")]
pub mod winit_adapter;

// Re-export core types
pub use history::History;
pub use input::{InputHandler, Selection};
pub use render::Renderer;
pub use syntax::SyntaxHighlighter;
pub use tiny_tree::{Content, Doc, Edit, Point, Rect, Span, Tree as DocTree};
