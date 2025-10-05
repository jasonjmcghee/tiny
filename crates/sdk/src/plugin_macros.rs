//! Declarative macros for eliminating plugin boilerplate

/// Comprehensive plugin declaration macro - reduces 200+ lines to ~20
///
/// # Example
/// ```ignore
/// plugin! {
///     CursorPlugin {
///         name: "cursor",
///         version: "0.1.0",
///         z_index: 10,
///         traits: [Init, Update, Paint, Library, Config],  // Declares capabilities
///         defaults: [Init, Paint],  // Auto-generate these (custom impl for Update, Library, Config)
///     }
/// }
/// ```
///
/// All traits in `traits` get capability + as_* method.
/// Only traits in `defaults` get auto-generated stub implementations.
/// If a trait is in `traits` but NOT in `defaults`, you must provide custom impl.
#[macro_export]
macro_rules! plugin {
    // Version with z_index and defaults
    (
        $plugin_type:ident {
            name: $name:expr,
            version: $version:expr,
            z_index: $z_index:expr,
            traits: [$($trait_name:ident),* $(,)?],
            defaults: [$($default_trait:ident),* $(,)?]
            $(,)?
        }
    ) => {
        impl $crate::Plugin for $plugin_type {
            fn name(&self) -> &str { $name }
            fn version(&self) -> &str { $version }
            fn capabilities(&self) -> Vec<$crate::Capability> {
                vec![$($crate::plugin!(@capability $trait_name, $name)),*]
            }
            $($crate::plugin!(@as_trait $trait_name);)*
        }
        $($crate::plugin!(@impl_trait $plugin_type, $default_trait, z_index: $z_index);)*
    };

    // Version without z_index but with defaults
    (
        $plugin_type:ident {
            name: $name:expr,
            version: $version:expr,
            traits: [$($trait_name:ident),* $(,)?],
            defaults: [$($default_trait:ident),* $(,)?]
            $(,)?
        }
    ) => {
        impl $crate::Plugin for $plugin_type {
            fn name(&self) -> &str { $name }
            fn version(&self) -> &str { $version }
            fn capabilities(&self) -> Vec<$crate::Capability> {
                vec![$($crate::plugin!(@capability $trait_name, $name)),*]
            }
            $($crate::plugin!(@as_trait $trait_name);)*
        }
        $($crate::plugin!(@impl_trait $plugin_type, $default_trait);)*
    };

    // Convert trait name to Capability
    (@capability Init, $name:expr) => { $crate::Capability::Initializable };
    (@capability Update, $name:expr) => { $crate::Capability::Updatable };
    (@capability Paint, $name:expr) => { $crate::Capability::Paintable($name.to_string()) };
    (@capability Library, $name:expr) => { $crate::Capability::Library(std::any::TypeId::of::<()>()) };
    (@capability Config, $name:expr) => { $crate::Capability::Initializable };

    // Generate as_* methods
    (@as_trait Init) => {
        fn as_initializable(&mut self) -> Option<&mut dyn $crate::Initializable> { Some(self) }
    };
    (@as_trait Update) => {
        fn as_updatable(&mut self) -> Option<&mut dyn $crate::Updatable> { Some(self) }
    };
    (@as_trait Paint) => {
        fn as_paintable(&self) -> Option<&dyn $crate::Paintable> { Some(self) }
    };
    (@as_trait Library) => {
        fn as_library(&self) -> Option<&dyn $crate::Library> { Some(self) }
        fn as_library_mut(&mut self) -> Option<&mut dyn $crate::Library> { Some(self) }
    };
    (@as_trait Config) => {
        fn as_configurable(&mut self) -> Option<&mut dyn $crate::Configurable> { Some(self) }
    };

    // Implement Initializable with default setup
    (@impl_trait $type:ty, Init) => {
        impl $crate::Initializable for $type {
            fn setup(&mut self, _ctx: &mut $crate::SetupContext) -> Result<(), $crate::PluginError> {
                Ok(())
            }
        }
    };
    (@impl_trait $type:ty, Init, z_index: $z:expr) => {
        impl $crate::Initializable for $type {
            fn setup(&mut self, _ctx: &mut $crate::SetupContext) -> Result<(), $crate::PluginError> {
                Ok(())
            }
        }
    };

    // Implement Paintable with z_index
    (@impl_trait $type:ty, Paint, z_index: $z_index:expr) => {
        impl $crate::Paintable for $type {
            fn paint(&self, _ctx: &$crate::PaintContext, _pass: &mut wgpu::RenderPass) {}
            fn z_index(&self) -> i32 { $z_index }
        }
    };
    (@impl_trait $type:ty, Paint) => {
        impl $crate::Paintable for $type {
            fn paint(&self, _ctx: &$crate::PaintContext, _pass: &mut wgpu::RenderPass) {}
            fn z_index(&self) -> i32 { 0 }
        }
    };

    // Skip Update, Library, Config - they need custom implementations
    (@impl_trait $type:ty, Update) => {};
    (@impl_trait $type:ty, Update, z_index: $z:expr) => {};
    (@impl_trait $type:ty, Library) => {};
    (@impl_trait $type:ty, Library, z_index: $z:expr) => {};
    (@impl_trait $type:ty, Config) => {};
    (@impl_trait $type:ty, Config, z_index: $z:expr) => {};
}
