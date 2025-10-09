//! Shortcut registry - maps accelerators to event names
//!
//! Supports context-aware shortcuts where the same accelerator can trigger
//! different events depending on the active context (e.g., file picker vs editor)

use crate::accelerator::{Accelerator, AcceleratorMatcher, Modifiers, Trigger};
use crate::input::EventBus;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// TOML configuration structure
#[derive(Debug, Default, Deserialize, Serialize)]
struct ShortcutsConfig {
    #[serde(default)]
    shortcuts: HashMap<String, ShortcutValue>,
}

/// A shortcut value can be either a single string or an array of strings
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
enum ShortcutValue {
    Single(String),
    Multiple(Vec<String>),
}

impl ShortcutValue {
    fn as_vec(&self) -> Vec<String> {
        match self {
            ShortcutValue::Single(s) => vec![s.clone()],
            ShortcutValue::Multiple(v) => v.clone(),
        }
    }
}

/// Maps accelerators to event names
pub struct ShortcutRegistry {
    /// Shortcuts map: accelerator -> list of event names
    shortcuts: Vec<(Accelerator, Vec<String>)>,
    /// Accelerator matcher for tracking sequences
    matcher: AcceleratorMatcher,
}

impl ShortcutRegistry {
    pub fn new() -> Self {
        let mut registry = Self {
            shortcuts: Vec::new(),
            matcher: AcceleratorMatcher::new(),
        };

        // Load shortcuts from file
        registry.load_shortcuts();
        registry
    }

    /// Reload shortcuts from shortcuts.toml
    pub fn reload(&mut self) {
        // Clear existing shortcuts
        self.shortcuts.clear();
        self.matcher.reset();

        // Load from file
        self.load_shortcuts();
    }

    /// Register a shortcut that triggers an event
    fn register(&mut self, accelerator: &str, event_name: impl Into<String>) {
        let acc = match Accelerator::parse(accelerator) {
            Ok(a) => a,
            Err(e) => {
                eprintln!("Failed to parse accelerator '{}': {}", accelerator, e);
                return;
            }
        };

        // Find existing entry or create new one
        if let Some((_, events)) = self.shortcuts.iter_mut().find(|(a, _)| a == &acc) {
            events.push(event_name.into());
        } else {
            self.shortcuts.push((acc, vec![event_name.into()]));
        }
    }

    /// Try to match an input against registered shortcuts
    /// Returns list of event names to emit
    pub fn match_input(&mut self, modifiers: &Modifiers, trigger: &Trigger) -> Vec<String> {
        // Collect all accelerators as candidates
        let candidates: Vec<Accelerator> =
            self.shortcuts.iter().map(|(acc, _)| acc.clone()).collect();

        // Try to match
        if let Some(matched) = self.matcher.feed(modifiers, trigger, &candidates) {
            // Find all events for this accelerator
            let mut events = Vec::new();
            for (acc, event_names) in &self.shortcuts {
                if acc == &matched {
                    events.extend(event_names.clone());
                }
            }
            return events;
        }

        Vec::new()
    }

    /// Load shortcuts from shortcuts.toml
    fn load_shortcuts(&mut self) {
        let config_path = PathBuf::from("shortcuts.toml");

        let config = match Self::load_config(&config_path) {
            Some(c) => c,
            None => {
                eprintln!("⚠️  Failed to load shortcuts.toml - no shortcuts will be available");
                return;
            }
        };

        // Register all shortcuts
        // Note: event_name is the key, shortcuts are the values
        for (event_name, shortcuts) in config.shortcuts {
            for accelerator in shortcuts.as_vec() {
                self.register(&accelerator, &event_name);
            }
        }
    }

    /// Load shortcuts configuration from a TOML file
    fn load_config(path: &Path) -> Option<ShortcutsConfig> {
        if !path.exists() {
            eprintln!(
                "ℹ️  No {} found - no shortcuts will be loaded",
                path.display()
            );
            return None;
        }

        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("❌ Failed to read {}: {}", path.display(), e);
                return None;
            }
        };

        match toml::from_str::<ShortcutsConfig>(&content) {
            Ok(config) => Some(config),
            Err(e) => {
                eprintln!("❌ TOML syntax error in {}: {}", path.display(), e);
                eprintln!("   No shortcuts will be loaded");
                None
            }
        }
    }
}

/// Helper to emit matched events to the event bus
pub fn emit_shortcut_events(events: Vec<String>, bus: &mut EventBus) {
    for event_name in events {
        bus.emit(event_name, json!({}), 10, "shortcuts");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_register_and_match() {
        let mut registry = ShortcutRegistry {
            shortcuts: Vec::new(),
            matcher: AcceleratorMatcher::new(),
        };
        registry.register("cmd+k", "test.event");

        let mut mods = Modifiers::default();
        mods.cmd = true;
        let events = registry.match_input(&mods, &Trigger::Char("k".to_string()));

        assert_eq!(events, vec!["test.event".to_string()]);
    }

    #[test]
    fn test_multiple_shortcuts_same_event() {
        let mut registry = ShortcutRegistry {
            shortcuts: Vec::new(),
            matcher: AcceleratorMatcher::new(),
        };
        registry.register("cmd+k", "test.event");
        registry.register("cmd+shift+k", "test.event");

        let mut mods = Modifiers::default();
        mods.cmd = true;
        let events = registry.match_input(&mods, &Trigger::Char("k".to_string()));

        assert_eq!(events, vec!["test.event".to_string()]);
    }

    #[test]
    fn test_load_from_toml() {
        // This test requires shortcuts.toml to exist
        let registry = ShortcutRegistry::new();

        // Verify that at least some shortcuts were loaded
        assert!(!registry.shortcuts.is_empty());
    }
}
