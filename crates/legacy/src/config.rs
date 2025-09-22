//! Configuration management for Tiny Editor

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AppConfig {
    #[serde(default)]
    pub editor: EditorConfig,
    #[serde(default)]
    pub plugins: PluginSystemConfig,
    #[serde(default)]
    pub development: DevelopmentConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct EditorConfig {
    #[serde(default = "default_font_size")]
    pub font_size: f32,
    #[serde(default = "default_theme")]
    pub theme: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PluginSystemConfig {
    #[serde(default = "default_plugin_dir")]
    pub plugin_dir: String,
    #[serde(default)]
    pub enabled: Vec<String>,

    // Individual plugin configs - parsed manually
    #[serde(skip)]
    pub plugins: HashMap<String, PluginConfig>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PluginConfig {
    /// Path to plugin library (optional, defaults to plugin_dir/lib{name}_plugin.{ext})
    pub lib: Option<String>,
    /// Path to plugin config (optional, defaults to plugin_dir/{name}.toml)
    pub config: Option<String>,
    /// Source directory for watching (required for auto_rebuild)
    pub source_dir: Option<String>,
    /// Build command (optional, defaults to cargo build)
    pub build_command: Option<String>,
    /// Watch source files and rebuild on changes
    #[serde(default)]
    pub auto_rebuild: bool,
    /// Hot-reload when lib changes
    #[serde(default)]
    pub auto_reload: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DevelopmentConfig {
    #[serde(default)]
    pub debug: bool,
    #[serde(default)]
    pub show_fps: bool,
}

impl Default for EditorConfig {
    fn default() -> Self {
        Self {
            font_size: default_font_size(),
            theme: default_theme(),
        }
    }
}

impl Default for PluginSystemConfig {
    fn default() -> Self {
        Self {
            plugin_dir: default_plugin_dir(),
            enabled: Vec::new(),
            plugins: HashMap::new(),
        }
    }
}

impl Default for DevelopmentConfig {
    fn default() -> Self {
        Self {
            debug: false,
            show_fps: false,
        }
    }
}

impl PluginConfig {
    /// Get the library path (with defaults)
    pub fn lib_path(&self, plugin_name: &str, plugin_dir: &str) -> String {
        self.lib.clone().unwrap_or_else(|| {
            #[cfg(target_os = "macos")]
            let ext = "dylib";
            #[cfg(target_os = "linux")]
            let ext = "so";
            #[cfg(target_os = "windows")]
            let ext = "dll";

            format!("{}/lib{}_plugin.{}", plugin_dir, plugin_name, ext)
        })
    }

    /// Get the config path (with defaults)
    pub fn config_path(&self, plugin_name: &str, plugin_dir: &str) -> String {
        self.config.clone().unwrap_or_else(|| {
            format!("{}/{}.toml", plugin_dir, plugin_name)
        })
    }
}

fn default_font_size() -> f32 { 14.0 }
fn default_theme() -> String { "dark".to_string() }
fn default_plugin_dir() -> String { "target/plugins/release".to_string() }

impl AppConfig {
    /// Load configuration from init.toml
    pub fn load() -> Result<Self, Box<dyn std::error::Error>> {
        let config_path = PathBuf::from("init.toml");

        if config_path.exists() {
            let content = std::fs::read_to_string(&config_path)?;

            // Parse the base config first
            let mut config: AppConfig = toml::from_str(&content)?;

            // Parse the TOML value to extract plugin sections
            let toml_value: toml::Value = toml::from_str(&content)?;

            // Extract individual plugin configs from [plugins.{name}] sections
            if let Some(plugins_table) = toml_value.get("plugins").and_then(|v| v.as_table()) {
                for (key, value) in plugins_table {
                    // Skip the main plugins fields (plugin_dir, enabled)
                    if key != "plugin_dir" && key != "enabled" {
                        if let Ok(plugin_config) = value.clone().try_into::<PluginConfig>() {
                            config.plugins.plugins.insert(key.clone(), plugin_config);
                            eprintln!("Loaded config for plugin: {}", key);
                        }
                    }
                }
            }

            eprintln!("Loaded configuration from init.toml with {} plugins", config.plugins.plugins.len());
            Ok(config)
        } else {
            eprintln!("No init.toml found, using defaults");
            Ok(AppConfig::default())
        }
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            editor: EditorConfig::default(),
            plugins: PluginSystemConfig::default(),
            development: DevelopmentConfig::default(),
        }
    }
}