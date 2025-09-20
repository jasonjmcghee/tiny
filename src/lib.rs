extern crate lazy_static;

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

pub mod app;
pub mod coordinates; // Coordinate system abstraction
pub mod font;
pub mod gpu;
pub mod history;
pub mod input;
pub mod io;
pub mod render;
pub mod syntax;
pub mod text_effects;
pub mod text_renderer;
pub mod theme;
pub mod tree;
pub mod widget;

// Re-export core types
pub use history::History;
pub use input::{InputHandler, Selection};
pub use render::Renderer;
pub use syntax::SyntaxHighlighter;
pub use tree::{Content, Doc, Edit, Point, Rect, Span, Tree};
pub use widget::{CursorWidget, SelectionWidget, StyleId, TextWidget};
pub use widget::{EventResponse, LayoutConstraints, LayoutResult, Widget, WidgetEvent, WidgetId};
