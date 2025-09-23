extern crate lazy_static;

// #[global_allocator]
// static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

// Host-only GPU FFI implementations
pub mod gpu_ffi_host;

#[cfg(feature = "winit")]
pub mod app;
pub mod config;
pub mod coordinates; // Coordinate system abstraction
pub mod history;
pub mod input;
pub mod input_types;
pub mod io;
pub mod render;
pub mod syntax;
pub mod text_effects;
pub mod text_renderer;
pub mod text_style_box_adapter;
pub mod theme;
pub mod widget;

// Re-export core types
pub use history::History;
pub use input::{InputHandler, Selection};
pub use render::Renderer;
pub use syntax::SyntaxHighlighter;
pub use tiny_tree::{Content, Doc, Edit, Point, Rect, Span, Tree as DocTree};
pub use widget::{StyleId, TextWidget};
pub use widget::{EventResponse, LayoutConstraints, LayoutResult, Widget, WidgetEvent, WidgetId};
