//! Plugin loading and management system
//!
//! Handles discovery, loading, and lifecycle of plugins from dynamic libraries

use ahash::AHashMap as HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tiny_sdk::{GlyphInstances, Hook, Paintable, Plugin, PluginError, Updatable};

/// Plugin configuration loaded from plugin.toml
#[derive(Debug, Clone)]
pub struct PluginConfig {
    pub name: String,
    pub version: String,
    pub description: String,
    pub entry_point: String,
    pub capabilities: PluginCapabilities,
    pub config: toml::Value,
}

#[derive(Debug, Clone, Default)]
pub struct PluginCapabilities {
    pub update: bool,
    pub paint: bool,
}

/// Loaded plugin instance
pub struct LoadedPlugin {
    /// Plugin configuration
    pub config: PluginConfig,
    /// The plugin instance
    pub instance: Box<dyn Plugin>,
    /// Dynamic library handle (kept alive)
    _lib: Option<libloading::Library>,
}

/// Plugin loader that manages dynamic libraries
pub struct PluginLoader {
    /// Path to plugin directory
    plugin_dir: PathBuf,
    /// Loaded plugins by name
    plugins: HashMap<String, LoadedPlugin>,
}

impl PluginLoader {
    /// Create a new plugin loader
    pub fn new(plugin_dir: PathBuf) -> Self {
        Self {
            plugin_dir,
            plugins: HashMap::new(),
        }
    }

    /// Discover and load all plugins in the plugin directory
    pub fn load_all(&mut self) -> Result<Vec<String>, PluginError> {
        let mut loaded = Vec::new();

        // Create plugin directory if it doesn't exist
        if !self.plugin_dir.exists() {
            std::fs::create_dir_all(&self.plugin_dir)
                .map_err(|e| PluginError::Other(Box::new(e)))?;
        }

        // Scan for plugin files
        let entries =
            std::fs::read_dir(&self.plugin_dir).map_err(|e| PluginError::Other(Box::new(e)))?;

        for entry in entries {
            let entry = entry.map_err(|e| PluginError::Other(Box::new(e)))?;
            let path = entry.path();

            // Look for .toml config files
            if path.extension().and_then(|s| s.to_str()) == Some("toml") {
                let name = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .ok_or_else(|| PluginError::Other("Invalid plugin name".into()))?;

                // Skip if already loaded
                if self.plugins.contains_key(name) {
                    continue;
                }

                // Try to load the plugin
                match self.load_plugin(name) {
                    Ok(_) => {
                        loaded.push(name.to_string());
                        println!("Loaded plugin: {}", name);
                    }
                    Err(e) => {
                        eprintln!("Failed to load plugin {}: {}", name, e);
                    }
                }
            }
        }

        Ok(loaded)
    }

    /// Load a specific plugin by name
    pub fn load_plugin(&mut self, name: &str) -> Result<(), PluginError> {
        // Load configuration
        let config_path = self.plugin_dir.join(format!("{}.toml", name));
        let config = self.load_config(&config_path)?;

        // Determine library file extension based on platform
        let lib_extension = if cfg!(target_os = "macos") {
            "dylib"
        } else if cfg!(target_os = "windows") {
            "dll"
        } else {
            "so"
        };

        // Load dynamic library
        let lib_path = self.plugin_dir.join(format!("{}.{}", name, lib_extension));
        let lib = unsafe {
            libloading::Library::new(&lib_path).map_err(|e| PluginError::Other(Box::new(e)))?
        };

        // Get entry point function
        let entry_point = config.entry_point.as_bytes();
        let create_fn: libloading::Symbol<fn() -> Box<dyn Plugin>> = unsafe {
            lib.get(entry_point)
                .map_err(|e| PluginError::Other(Box::new(e)))?
        };

        // Create plugin instance
        let instance = create_fn();

        // Store the loaded plugin
        self.plugins.insert(
            name.to_string(),
            LoadedPlugin {
                config,
                instance,
                _lib: Some(lib),
            },
        );

        Ok(())
    }

    /// Load plugin configuration from TOML file
    fn load_config(&self, path: &Path) -> Result<PluginConfig, PluginError> {
        let contents =
            std::fs::read_to_string(path).map_err(|e| PluginError::Other(Box::new(e)))?;

        let toml: toml::Value =
            toml::from_str(&contents).map_err(|e| PluginError::Other(Box::new(e)))?;

        // Parse metadata
        let metadata = toml
            .get("metadata")
            .ok_or_else(|| PluginError::Other("Missing [metadata] section".into()))?;

        let name = metadata
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| PluginError::Other("Missing metadata.name".into()))?
            .to_string();

        let version = metadata
            .get("version")
            .and_then(|v| v.as_str())
            .unwrap_or("0.0.0")
            .to_string();

        let description = metadata
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        // Parse capabilities
        let capabilities = if let Some(cap) = toml.get("capabilities") {
            PluginCapabilities {
                update: cap.get("update").and_then(|v| v.as_bool()).unwrap_or(false),
                paint: cap.get("paint").and_then(|v| v.as_bool()).unwrap_or(false),
            }
        } else {
            PluginCapabilities::default()
        };

        // Parse exports
        let entry_point = toml
            .get("exports")
            .and_then(|e| e.get("entry_point"))
            .and_then(|v| v.as_str())
            .unwrap_or("plugin_create")
            .to_string();

        // Get config section (if any)
        let config = toml
            .get("config")
            .cloned()
            .unwrap_or(toml::Value::Table(toml::value::Table::new()));

        Ok(PluginConfig {
            name,
            version,
            description,
            entry_point,
            capabilities,
            config,
        })
    }

    /// Get all plugins that implement Update
    pub fn get_update_plugins(&mut self) -> Vec<&mut dyn Updatable> {
        let mut updates = Vec::new();
        for plugin in self.plugins.values_mut() {
            if plugin.config.capabilities.update {
                if let Some(update_plugin) = plugin.instance.as_updatable() {
                    updates.push(update_plugin);
                }
            }
        }
        updates
    }

    /// Get all plugins that implement Paint
    pub fn get_paint_plugins(&self) -> Vec<&dyn Paintable> {
        let mut paints = Vec::new();
        for plugin in self.plugins.values() {
            if plugin.config.capabilities.paint {
                if let Some(paint_plugin) = plugin.instance.as_paintable() {
                    paints.push(paint_plugin);
                }
            }
        }
        paints
    }

    /// Get all plugins that implement Hook for GlyphInstances
    pub fn get_glyph_hooks(&self) -> Vec<&dyn Hook<GlyphInstances, Output = GlyphInstances>> {
        let mut hooks = Vec::new();
        for plugin in self.plugins.values() {
            if let Some(hook) = plugin.instance.as_glyph_hook() {
                hooks.push(hook);
            }
        }
        hooks
    }

    /// Hot-reload a plugin
    pub fn reload_plugin(&mut self, name: &str) -> Result<(), PluginError> {
        // Remove old plugin
        self.plugins.remove(name);

        // Load new version
        self.load_plugin(name)
    }

    /// Unload a plugin
    pub fn unload_plugin(&mut self, name: &str) -> Option<LoadedPlugin> {
        self.plugins.remove(name)
    }

    /// Get plugin by name
    pub fn get_plugin(&self, name: &str) -> Option<&LoadedPlugin> {
        self.plugins.get(name)
    }

    /// Get mutable plugin by name
    pub fn get_plugin_mut(&mut self, name: &str) -> Option<&mut LoadedPlugin> {
        self.plugins.get_mut(name)
    }

    /// List all loaded plugins
    pub fn list_plugins(&self) -> Vec<String> {
        self.plugins.keys().cloned().collect()
    }
}

/// File watcher for hot-reloading plugins
pub struct PluginWatcher {
    loader: Arc<std::sync::Mutex<PluginLoader>>,
    _watcher: notify::RecommendedWatcher,
}

impl PluginWatcher {
    /// Create a new plugin watcher
    pub fn new(loader: Arc<std::sync::Mutex<PluginLoader>>) -> Result<Self, PluginError> {
        use notify::{Event, RecursiveMode, Watcher};

        let loader_clone = loader.clone();
        let mut watcher = notify::recommended_watcher(move |res: Result<Event, notify::Error>| {
            if let Ok(event) = res {
                // Only care about modifications to plugin files
                if event.kind.is_modify() {
                    for path in &event.paths {
                        if let Some(ext) = path.extension() {
                            if ext == "dylib" || ext == "so" || ext == "dll" {
                                // Extract plugin name
                                if let Some(name) = path.file_stem().and_then(|s| s.to_str()) {
                                    // Trigger reload
                                    if let Ok(mut loader) = loader_clone.lock() {
                                        println!("Reloading plugin: {}", name);
                                        if let Err(e) = loader.reload_plugin(name) {
                                            eprintln!("Failed to reload plugin {}: {}", name, e);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        })
        .map_err(|e| PluginError::Other(Box::new(e)))?;

        // Watch plugin directory
        if let Ok(loader) = loader.lock() {
            watcher
                .watch(&loader.plugin_dir, RecursiveMode::NonRecursive)
                .map_err(|e| PluginError::Other(Box::new(e)))?;
        }

        Ok(Self {
            loader,
            _watcher: watcher,
        })
    }
}
