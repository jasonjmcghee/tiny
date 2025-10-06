//! Plugin loading and management system
//!
//! Handles discovery, loading, and lifecycle of plugins from dynamic libraries

use ahash::{AHashMap as HashMap, AHashSet as HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tiny_sdk::{GlyphInstances, Hook, Library, Paintable, Plugin, PluginError, Updatable};

/// Plugin configuration loaded from plugin.toml
#[derive(Debug, Clone)]
pub struct PluginConfig {
    pub name: String,
    pub version: String,
    pub description: String,
    pub entry_point: String,
    pub capabilities: PluginCapabilities,
    pub dependencies: Vec<String>,
    pub config: toml::Value,
}

#[derive(Debug, Clone, Default)]
pub struct PluginCapabilities {
    pub update: bool,
    pub paint: bool,
    pub library: bool,
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
    /// Shared registry for all plugins
    registry: tiny_sdk::PluginRegistry,
    /// GPU device for plugin initialization (set after first init)
    device: Option<std::sync::Arc<wgpu::Device>>,
    /// GPU queue for plugin initialization (set after first init)
    queue: Option<std::sync::Arc<wgpu::Queue>>,
}

impl PluginLoader {
    /// Create a new plugin loader
    pub fn new(plugin_dir: PathBuf) -> Self {
        Self {
            plugin_dir,
            plugins: HashMap::new(),
            registry: tiny_sdk::PluginRegistry::empty(),
            device: None,
            queue: None,
        }
    }

    /// Resolve plugin load order based on dependencies
    fn resolve_load_order(
        &self,
        plugin_configs: &HashMap<String, PluginConfig>,
    ) -> Result<Vec<String>, PluginError> {
        let mut loaded = HashSet::new();
        let mut order = Vec::new();

        // Helper function to recursively load dependencies
        fn visit(
            name: &str,
            configs: &HashMap<String, PluginConfig>,
            loaded: &mut HashSet<String>,
            order: &mut Vec<String>,
            visiting: &mut HashSet<String>,
        ) -> Result<(), PluginError> {
            if loaded.contains(name) {
                return Ok(());
            }

            if visiting.contains(name) {
                return Err(PluginError::Other(
                    format!("Circular dependency detected involving plugin: {}", name).into(),
                ));
            }

            visiting.insert(name.to_string());

            if let Some(config) = configs.get(name) {
                // Load dependencies first
                for dep in &config.dependencies {
                    visit(dep, configs, loaded, order, visiting)?;
                }

                // Then load this plugin
                if !loaded.contains(name) {
                    loaded.insert(name.to_string());
                    order.push(name.to_string());
                }
            }

            visiting.remove(name);
            Ok(())
        }

        let mut visiting = HashSet::new();
        for name in plugin_configs.keys() {
            visit(name, plugin_configs, &mut loaded, &mut order, &mut visiting)?;
        }

        Ok(order)
    }

    /// Discover and load all plugins in the plugin directory
    pub fn load_all(&mut self) -> Result<Vec<String>, PluginError> {
        // Create plugin directory if it doesn't exist
        if !self.plugin_dir.exists() {
            std::fs::create_dir_all(&self.plugin_dir)
                .map_err(|e| PluginError::Other(Box::new(e)))?;
        }

        // First, discover all plugin configs
        let mut plugin_configs = HashMap::new();
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

                // Load config to check dependencies
                match self.load_config(&path) {
                    Ok(config) => {
                        plugin_configs.insert(name.to_string(), config);
                    }
                    Err(e) => {
                        eprintln!("Failed to load config for plugin {}: {}", name, e);
                    }
                }
            }
        }

        // Resolve load order based on dependencies
        let load_order = self.resolve_load_order(&plugin_configs)?;
        let mut loaded = Vec::new();

        // Load plugins in dependency order
        for name in load_order {
            if self.plugins.contains_key(&name) {
                continue;
            }

            match self.load_plugin(&name) {
                Ok(_) => {
                    loaded.push(name.to_string());
                    println!("Loaded plugin: {}", name);
                }
                Err(e) => {
                    eprintln!("Failed to load plugin {}: {}", name, e);
                }
            }
        }

        Ok(loaded)
    }

    /// Load a plugin from explicit paths
    pub fn load_plugin_from_path(
        &mut self,
        name: &str,
        lib_path: &str,
        config_path: &str,
    ) -> Result<(), PluginError> {
        // Load configuration from explicit path
        let config_path_buf = std::path::PathBuf::from(config_path);
        let config = self.load_config(&config_path_buf)?;

        // Read the config file content for the plugin
        let config_content = if config_path_buf.exists() {
            std::fs::read_to_string(&config_path_buf).ok()
        } else {
            None
        };

        // Load dynamic library from explicit path
        let lib_path_buf = std::path::PathBuf::from(lib_path);

        // IMPORTANT: Load the plugin with RTLD_GLOBAL so it can access host symbols
        let lib = unsafe {
            #[cfg(unix)]
            {
                use libloading::os::unix::Library as UnixLibrary;

                const RTLD_LAZY: std::os::raw::c_int = 0x1;
                const RTLD_GLOBAL: std::os::raw::c_int = 0x100;

                let unix_lib = UnixLibrary::open(Some(&lib_path_buf), RTLD_LAZY | RTLD_GLOBAL)
                    .map_err(|e| PluginError::Other(Box::new(e)))?;
                libloading::Library::from(unix_lib)
            }

            #[cfg(not(unix))]
            {
                libloading::Library::new(&lib_path_buf)
                    .map_err(|e| PluginError::Other(Box::new(e)))?
            }
        };

        // Get entry point function
        let entry_point = config.entry_point.as_bytes();
        let create_fn: libloading::Symbol<fn() -> Box<dyn Plugin>> = unsafe {
            lib.get(entry_point)
                .map_err(|e| PluginError::Other(Box::new(e)))?
        };

        // Create plugin instance
        let mut instance = create_fn();

        // Send initial config to the plugin if it supports it
        if let Some(ref config_str) = config_content {
            if let Some(configurable) = instance.as_configurable() {
                if let Err(e) = configurable.config_updated(config_str) {
                    eprintln!(
                        "Warning: Failed to apply initial config to plugin {}: {}",
                        name, e
                    );
                } else {
                    eprintln!("Applied initial config to plugin {}", name);
                }
            }
        }

        // Store the loaded plugin
        self.plugins.insert(
            name.to_string(),
            LoadedPlugin {
                config,
                instance,
                _lib: Some(lib),
            },
        );

        eprintln!("Plugin loaded from explicit path: {}", name);
        Ok(())
    }

    /// Create a new instance of an already-loaded plugin
    /// This allows multiple instances of the same plugin (e.g., multiple cursors)
    /// The library must already be loaded for this to work
    pub fn create_plugin_instance(&self, name: &str) -> Result<Box<dyn Plugin>, PluginError> {
        let loaded = self.plugins.get(name).ok_or_else(|| {
            PluginError::Other(
                format!("Plugin '{}' not loaded. Load it first with load_plugin.", name).into(),
            )
        })?;

        // Get the library handle - plugins keep the lib alive
        if let Some(ref lib) = loaded._lib {
            // Get entry point function
            let entry_point = loaded.config.entry_point.as_bytes();
            let create_fn: libloading::Symbol<fn() -> Box<dyn Plugin>> = unsafe {
                lib.get(entry_point)
                    .map_err(|e| PluginError::Other(Box::new(e)))?
            };

            // Create a NEW instance
            let mut instance = create_fn();

            // Apply config from the loaded plugin
            if let Some(configurable) = instance.as_configurable() {
                // Wrap config in [config] section before serializing
                let mut config_toml = toml::value::Table::new();
                config_toml.insert("config".to_string(), loaded.config.config.clone());
                let config_str = toml::to_string(&config_toml)
                    .expect("Failed to serialize plugin config to TOML");
                if !config_str.is_empty() {
                    configurable.config_updated(&config_str)
                        .map_err(|e| PluginError::Other(format!("Failed to apply plugin config: {}", e).into()))?;
                }
            }

            Ok(instance)
        } else {
            Err(PluginError::Other(
                format!("Plugin '{}' has no library handle", name).into(),
            ))
        }
    }

    /// Load a specific plugin by name using default paths
    /// Note: You must call initialize_plugin separately with GPU resources
    pub fn load_plugin(&mut self, name: &str) -> Result<(), PluginError> {
        // Determine library file extension based on platform
        let lib_extension = if cfg!(target_os = "macos") {
            "dylib"
        } else if cfg!(target_os = "windows") {
            "dll"
        } else {
            "so"
        };

        // Build default paths
        let lib_path = self.plugin_dir.join(format!(
            "lib{}_plugin.{}",
            name.replace("-", "_"),
            lib_extension
        ));
        let config_path = self.plugin_dir.join(format!("{}.toml", name));

        // Use the explicit path loader
        self.load_plugin_from_path(
            name,
            lib_path.to_str().unwrap(),
            config_path.to_str().unwrap(),
        )
    }

    /// Initialize a plugin with GPU resources after loading
    pub fn initialize_plugin(
        &mut self,
        name: &str,
        device: std::sync::Arc<wgpu::Device>,
        queue: std::sync::Arc<wgpu::Queue>,
    ) -> Result<(), PluginError> {
        // Store device and queue for future use (e.g., reloading)
        if self.device.is_none() {
            self.device = Some(device.clone());
            self.queue = Some(queue.clone());
        }

        let plugin = self
            .plugins
            .get_mut(name)
            .ok_or_else(|| PluginError::Other(format!("Plugin {} not found", name).into()))?;

        if let Some(initializable) = plugin.instance.as_initializable() {
            eprintln!("Initializing plugin: {}", name);
            let mut ctx = tiny_sdk::SetupContext {
                device,
                queue,
                registry: self.registry.clone(), // Use shared registry
            };
            initializable.setup(&mut ctx)?;
        }

        Ok(())
    }

    /// Unload a plugin, calling cleanup if needed
    pub fn unload_plugin(&mut self, name: &str) -> Result<(), PluginError> {
        if let Some(mut plugin) = self.plugins.remove(name) {
            // Call cleanup if the plugin implements it
            if let Some(initializable) = plugin.instance.as_initializable() {
                eprintln!("Cleaning up plugin: {}", name);
                initializable.cleanup()?;
            }
            eprintln!("Unloaded plugin: {}", name);
        }
        Ok(())
    }

    /// Reload a plugin (unload, then load and initialize)
    /// Requires GPU resources to have been previously set via initialize_plugin
    pub fn reload_plugin(&mut self, name: &str) -> Result<(), PluginError> {
        eprintln!("Reloading plugin: {}", name);

        // Get stored device and queue
        let device = self.device.clone().ok_or_else(|| {
            PluginError::Other("No GPU device available - initialize a plugin first".into())
        })?;
        let queue = self.queue.clone().ok_or_else(|| {
            PluginError::Other("No GPU queue available - initialize a plugin first".into())
        })?;

        // First unload the existing plugin
        self.unload_plugin(name)?;

        // Then load it again
        self.load_plugin(name)?;

        // And initialize with GPU resources
        self.initialize_plugin(name, device, queue)?;

        eprintln!("Successfully reloaded plugin: {}", name);
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
                library: cap
                    .get("library")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
            }
        } else {
            PluginCapabilities::default()
        };

        // Parse dependencies
        let dependencies = if let Some(deps) = toml.get("dependencies") {
            deps.as_table()
                .map(|table| table.keys().cloned().collect())
                .unwrap_or_else(Vec::new)
        } else {
            Vec::new()
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
            dependencies,
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

    /// Get plugin by name
    pub fn get_plugin(&self, name: &str) -> Option<&LoadedPlugin> {
        self.plugins.get(name)
    }

    /// Get mutable plugin by name
    pub fn get_plugin_mut(&mut self, name: &str) -> Option<&mut LoadedPlugin> {
        self.plugins.get_mut(name)
    }

    /// Get plugin's dependencies as Library implementations
    pub fn get_plugin_dependencies(&self, name: &str) -> Vec<&dyn Library> {
        let mut dependencies = Vec::new();

        if let Some(plugin) = self.plugins.get(name) {
            for dep_name in &plugin.config.dependencies {
                if let Some(dep_plugin) = self.plugins.get(dep_name) {
                    if let Some(library) = dep_plugin.instance.as_library() {
                        dependencies.push(library);
                    }
                }
            }
        }

        dependencies
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
    _source_watcher: Option<notify::RecommendedWatcher>,
    auto_rebuild: bool,
    build_command: Option<String>,
}

impl PluginWatcher {
    /// Enable source file watching with automatic rebuild
    pub fn enable_source_watching(
        &mut self,
        watch_dirs: Vec<PathBuf>,
        build_command: String,
    ) -> Result<(), PluginError> {
        use notify::{Event, RecursiveMode, Watcher};
        use std::process::Command;

        self.auto_rebuild = true;
        self.build_command = Some(build_command.clone());

        let _loader = self.loader.clone();
        let build_cmd = build_command;

        let mut source_watcher =
            notify::recommended_watcher(move |res: Result<Event, notify::Error>| {
                if let Ok(event) = res {
                    // Watch for source file changes
                    if event.kind.is_modify() || event.kind.is_create() {
                        for path in &event.paths {
                            if let Some(ext) = path.extension() {
                                // Watch Rust source files and TOML configs
                                if ext == "rs" || ext == "toml" {
                                    eprintln!("Source file changed: {:?}", path);

                                    // Trigger rebuild
                                    eprintln!("Rebuilding plugins: {}", build_cmd);
                                    let output =
                                        Command::new("sh").arg("-c").arg(&build_cmd).output();

                                    match output {
                                        Ok(output) => {
                                            if output.status.success() {
                                                eprintln!("Build successful!");
                                                // The dylib watcher will pick up the change and reload
                                            } else {
                                                eprintln!(
                                                    "Build failed: {}",
                                                    String::from_utf8_lossy(&output.stderr)
                                                );
                                            }
                                        }
                                        Err(e) => eprintln!("Failed to run build command: {}", e),
                                    }
                                    break; // Only build once per event
                                }
                            }
                        }
                    }
                }
            })
            .map_err(|e| PluginError::Other(Box::new(e)))?;

        // Watch all specified directories
        for dir in watch_dirs {
            eprintln!("Watching source directory: {:?}", dir);
            source_watcher
                .watch(&dir, RecursiveMode::Recursive)
                .map_err(|e| PluginError::Other(Box::new(e)))?;
        }

        self._source_watcher = Some(source_watcher);
        Ok(())
    }

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
                                        // Reload using stored GPU resources
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
            _source_watcher: None,
            auto_rebuild: false,
            build_command: None,
        })
    }
}
