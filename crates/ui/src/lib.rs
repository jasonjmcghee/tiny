//! Tiny UI - Reusable UI components for plugins
//!
//! This crate provides common UI components that plugins can use:
//! - TextView: Universal text display with optional interactivity
//! - Coordinates: Viewport and coordinate transformation utilities
//! - TextRenderer: Text layout and rendering
//! - Scroll: Scrolling trait
//! - Syntax: Syntax highlighting
//! - Theme: Color theming
//! - TextEffects: Text styling and effects
//! - Capabilities: Feature flags for TextView/EditableTextView
//!
//! More complex components (EditableTextView, FilterableDropdown, etc.) are in tiny-editor.

pub mod capabilities;
pub mod coordinates;
pub mod scroll;
pub mod syntax;
pub mod text_effects;
pub mod text_renderer;
pub mod text_view;
pub mod theme;
pub mod widget;

// Re-export common types
pub use capabilities::TextViewCapabilities;
pub use coordinates::Viewport;
pub use scroll::Scrollable;
pub use syntax::SyntaxHighlighter;
pub use text_renderer::TextRenderer;
pub use text_view::{ArrowDirection, SizeConstraint, TextAlign, TextView};
pub use theme::Theme;
pub use widget::Widget;
