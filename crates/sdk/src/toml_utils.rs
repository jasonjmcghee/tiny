//! Resilient TOML parsing utilities
//!
//! Provides graceful error handling for TOML configuration files:
//! - Syntax errors → use defaults
//! - Type errors → use defaults for bad fields, keep good ones
//! - Missing files → use defaults
//!
//! # Example Usage (Plugin Config)
//!
//! ```ignore
//! use tiny_sdk::toml_utils;
//! use serde::Deserialize;
//!
//! #[derive(Default, Deserialize)]
//! struct MyPluginConfig {
//!     enabled: bool,
//!     speed: f32,
//!     color: String,
//! }
//!
//! impl Configurable for MyPlugin {
//!     fn config_updated(&mut self, toml_str: &str) -> Result<(), PluginError> {
//!         // Parse as TOML value first
//!         let toml_value: toml::Value = toml::from_str(toml_str)
//!             .map_err(|e| {
//!                 eprintln!("❌ Invalid TOML: {}", e);
//!                 PluginError::ConfigError
//!             })?;
//!
//!         // Parse fields individually - bad fields use defaults, good ones work
//!         let mut config = MyPluginConfig::default();
//!         if let Some(table) = toml_value.as_table() {
//!             tiny_sdk::parse_fields!(config, table, {
//!                 enabled: true,
//!                 speed: 1.0,
//!                 color: "white".to_string(),
//!             });
//!         }
//!
//!         self.apply_config(config);
//!         Ok(())
//!     }
//! }
//! ```

use serde::de::DeserializeOwned;
use std::path::Path;

/// Result of loading a TOML file
pub enum TomlLoadResult<T> {
    /// Successfully loaded and parsed
    Loaded(T),
    /// File doesn't exist, using defaults
    NotFound(T),
    /// Syntax error in TOML, using defaults
    SyntaxError { error: String, defaults: T },
}

/// Load a TOML file with graceful error handling
///
/// Returns the parsed config or defaults if anything goes wrong.
/// Prints warnings for errors but never panics or fails.
///
/// # Example
/// ```ignore
/// let config = toml_utils::load_or_default::<MyConfig>("config.toml");
/// ```
pub fn load_or_default<T: Default + DeserializeOwned>(path: impl AsRef<Path>) -> T {
    match load(path.as_ref()) {
        TomlLoadResult::Loaded(config) => config,
        TomlLoadResult::NotFound(defaults) => {
            eprintln!("ℹ️  No {} found - using defaults", path.as_ref().display());
            defaults
        }
        TomlLoadResult::SyntaxError { error, defaults } => {
            eprintln!("❌ TOML syntax error in {}: {}", path.as_ref().display(), error);
            eprintln!("   Using default configuration");
            defaults
        }
    }
}

/// Load a TOML file with detailed error information
pub fn load<T: Default + DeserializeOwned>(path: &Path) -> TomlLoadResult<T> {
    if !path.exists() {
        return TomlLoadResult::NotFound(T::default());
    }

    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("❌ Failed to read {}: {}", path.display(), e);
            return TomlLoadResult::NotFound(T::default());
        }
    };

    match toml::from_str::<T>(&content) {
        Ok(config) => TomlLoadResult::Loaded(config),
        Err(e) => TomlLoadResult::SyntaxError {
            error: e.to_string(),
            defaults: T::default(),
        },
    }
}

/// Parse a TOML value tree into a typed struct
///
/// Use this when you want to parse individual sections from a pre-parsed TOML value.
/// Returns None if parsing fails.
///
/// # Example
/// ```ignore
/// let toml_value: toml::Value = toml::from_str(&content)?;
/// if let Some(editor) = toml_utils::parse_section::<EditorConfig>(&toml_value, "editor") {
///     // Use editor config
/// }
/// ```
pub fn parse_section<T: DeserializeOwned>(
    toml_value: &toml::Value,
    section_name: &str,
) -> Option<T> {
    let section = toml_value.get(section_name)?;
    match section.clone().try_into() {
        Ok(v) => Some(v),
        Err(e) => {
            eprintln!(
                "⚠️  Error in [{}] section: {}. Using defaults for this section.",
                section_name, e
            );
            None
        }
    }
}

/// Parse a single field from a TOML table with a default fallback
///
/// Returns the parsed value or the default if parsing fails.
///
/// # Example
/// ```ignore
/// let table = toml_value.get("editor").and_then(|v| v.as_table())?;
/// let font_size: f32 = parse_field(table, "font_size", 14.0);
/// ```
pub fn parse_field<T: DeserializeOwned>(
    table: &toml::value::Table,
    field_name: &str,
    default: T,
) -> T {
    match table.get(field_name) {
        Some(value) => match value.clone().try_into() {
            Ok(v) => v,
            Err(e) => {
                eprintln!(
                    "⚠️  Invalid value for {}: {}. Using default.",
                    field_name, e
                );
                default
            }
        },
        None => default,
    }
}

/// Macro to parse multiple fields from a TOML table with defaults
///
/// # Example
/// ```ignore
/// parse_fields!(config.editor, table, {
///     window_title: default_window_title(),
///     font_size: default_font_size(),
///     font_italic: false,
/// });
/// ```
#[macro_export]
macro_rules! parse_fields {
    ($target:expr, $table:expr, { $($field:ident: $default:expr),* $(,)? }) => {
        $(
            if let Some(value) = $table.get(stringify!($field)) {
                match value.clone().try_into() {
                    Ok(v) => $target.$field = v,
                    Err(e) => {
                        eprintln!("⚠️  Invalid value for {}: {}. Using default.", stringify!($field), e);
                        $target.$field = $default;
                    }
                }
            }
        )*
    };
}
