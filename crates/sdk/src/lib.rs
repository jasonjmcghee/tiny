//! Tiny Editor Plugin SDK
//!
//! This crate defines the minimal interface for creating plugins.
//! Everything a plugin needs to integrate with the system is here.
//!
//! The five core traits:
//! - `Initializable` - One-time initialization
//! - `Updatable` - Per-frame logic updates
//! - `Paintable` - Per-frame rendering
//! - `Library` - Expose functionality to other plugins
//! - `Hook<T>` - Transform data flowing through the system

// Re-export all traits
pub use crate::traits::{
    Capability, Configurable, Hook, Initializable, Library, PaintContext, Paintable, Plugin,
    PluginError, PluginRegistry, SetupContext, Spatial, Updatable, UpdateContext,
};

// Re-export all types
pub use crate::types::{
    ByteRange, Color, DocPos, GlyphInstance, GlyphInstances, InputEvent, KeyEvent, LayoutPos,
    LayoutRect, LogicalPixels, LogicalPos, LogicalRect, LogicalSize, Modifiers, MouseButton,
    MouseEvent, PhysicalPixels, PhysicalPos, PhysicalSize, PhysicalSizeF, ScrollEvent, TokenType, ViewPos,
    ViewRect, ViewportInfo, WidgetViewport,
};

// Re-export services
pub use crate::services::{
    ContextData, FontService, PositionedGlyph, ServiceRegistry, TextEffect, TextLayout, TextStyleService,
};

pub use bytemuck;
pub use bytemuck::{Pod, Zeroable};
pub use wgpu;

mod traits;
pub mod types;
pub mod services;
pub mod ffi;

/// Macro to simplify plugin declaration
///
/// Example:
/// ```
/// declare_plugin!(
///     MyPlugin,
///     version = "0.1.0",
///     capabilities = [Paint("text"), Library(TextAPI)]
/// );
/// ```
#[macro_export]
macro_rules! declare_plugin {
    ($name:ident, version = $version:expr, capabilities = [$($cap:expr),*]) => {
        impl $crate::Plugin for $name {
            fn name(&self) -> &str {
                stringify!($name)
            }

            fn version(&self) -> &str {
                $version
            }

            fn capabilities(&self) -> Vec<$crate::Capability> {
                vec![$($cap),*]
            }

            fn as_any(&self) -> &dyn std::any::Any {
                self
            }

            fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
                self
            }
        }
    };
}
