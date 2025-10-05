//! Shortcut registry - maps accelerators to event names
//!
//! Supports context-aware shortcuts where the same accelerator can trigger
//! different events depending on the active context (e.g., file picker vs editor)

use crate::accelerator::{Accelerator, AcceleratorMatcher, Modifiers, Trigger};
use crate::input::EventBus;
use serde_json::json;
use std::collections::HashMap;

/// Context for shortcut matching (determines which shortcuts are active)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShortcutContext {
    /// File picker is active
    FilePicker,
    /// Grep search is active
    Grep,
    /// Editor is active (default)
    Editor,
    /// Global shortcuts (always active)
    Global,
}

/// Maps accelerators to event names (with context support)
pub struct ShortcutRegistry {
    /// Shortcuts grouped by context
    shortcuts: HashMap<ShortcutContext, Vec<(Accelerator, Vec<String>)>>,
    /// Current active context
    current_context: ShortcutContext,
    /// Accelerator matcher for tracking sequences
    matcher: AcceleratorMatcher,
}

impl ShortcutRegistry {
    pub fn new() -> Self {
        let mut registry = Self {
            shortcuts: HashMap::new(),
            current_context: ShortcutContext::Editor,
            matcher: AcceleratorMatcher::new(),
        };

        // Register default shortcuts
        registry.register_defaults();
        registry
    }

    /// Register a shortcut that triggers an event
    pub fn register(
        &mut self,
        context: ShortcutContext,
        accelerator: &str,
        event_name: impl Into<String>,
    ) {
        let acc = match Accelerator::parse(accelerator) {
            Ok(a) => a,
            Err(e) => {
                eprintln!("Failed to parse accelerator '{}': {}", accelerator, e);
                return;
            }
        };

        let shortcuts = self.shortcuts.entry(context).or_insert_with(Vec::new);

        // Find existing entry or create new one
        if let Some((_, events)) = shortcuts.iter_mut().find(|(a, _)| a == &acc) {
            events.push(event_name.into());
        } else {
            shortcuts.push((acc, vec![event_name.into()]));
        }
    }

    /// Set the active context
    pub fn set_context(&mut self, context: ShortcutContext) {
        self.current_context = context;
        self.matcher.reset(); // Reset matcher when context changes
    }

    /// Get the current context
    pub fn context(&self) -> ShortcutContext {
        self.current_context
    }

    /// Try to match an input against registered shortcuts
    /// Returns list of event names to emit
    pub fn match_input(&mut self, modifiers: &Modifiers, trigger: &Trigger) -> Vec<String> {
        // Collect all candidates from current context and global
        let mut candidates = Vec::new();

        // Add global shortcuts
        if let Some(global) = self.shortcuts.get(&ShortcutContext::Global) {
            for (acc, _) in global {
                candidates.push(acc.clone());
            }
        }

        // Add context-specific shortcuts
        if let Some(context) = self.shortcuts.get(&self.current_context) {
            for (acc, _) in context {
                candidates.push(acc.clone());
            }
        }

        // Try to match
        if let Some(matched) = self.matcher.feed(modifiers, trigger, &candidates) {
            // Find all events for this accelerator
            let mut events = Vec::new();

            // Check global
            if let Some(global) = self.shortcuts.get(&ShortcutContext::Global) {
                for (acc, event_names) in global {
                    if acc == &matched {
                        events.extend(event_names.clone());
                    }
                }
            }

            // Check context
            if let Some(context) = self.shortcuts.get(&self.current_context) {
                for (acc, event_names) in context {
                    if acc == &matched {
                        events.extend(event_names.clone());
                    }
                }
            }

            return events;
        }

        Vec::new()
    }

    /// Register default shortcuts from static table
    fn register_defaults(&mut self) {
        use ShortcutContext::*;

        // Static table of (context, accelerator, event_name)
        const SHORTCUTS: &[(&str, &str, &str)] = &[
            // Global shortcuts
            ("Global", "cmd+=", "app.font_increase"),
            ("Global", "cmd+-", "app.font_decrease"),
            ("Global", "f12", "app.toggle_scroll_lock"),

            // Editor shortcuts
            ("Editor", "cmd+s", "editor.save"),
            ("Editor", "cmd+z", "editor.undo"),
            ("Editor", "cmd+shift+z", "editor.redo"),
            ("Editor", "cmd+c", "editor.copy"),
            ("Editor", "cmd+x", "editor.cut"),
            ("Editor", "cmd+v", "editor.paste"),
            ("Editor", "cmd+a", "editor.select_all"),
            ("Editor", "cmd+b", "navigation.goto_definition"),
            ("Editor", "cmd+[", "navigation.back"),
            ("Editor", "cmd+]", "navigation.forward"),
            ("Editor", "cmd+w", "tabs.close"),
            ("Editor", "alt+enter", "editor.code_action"),
            ("Editor", "enter", "editor.insert_newline"),
            ("Editor", "tab", "editor.insert_tab"),
            ("Editor", "space", "editor.insert_space"),
            ("Editor", "backspace", "editor.delete_backward"),
            ("Editor", "delete", "editor.delete_forward"),
            ("Editor", "left", "editor.move_left"),
            ("Editor", "right", "editor.move_right"),
            ("Editor", "up", "editor.move_up"),
            ("Editor", "down", "editor.move_down"),
            ("Editor", "shift+left", "editor.extend_left"),
            ("Editor", "shift+right", "editor.extend_right"),
            ("Editor", "shift+up", "editor.extend_up"),
            ("Editor", "shift+down", "editor.extend_down"),
            ("Editor", "home", "editor.move_line_start"),
            ("Editor", "end", "editor.move_line_end"),
            ("Editor", "shift+home", "editor.extend_line_start"),
            ("Editor", "shift+end", "editor.extend_line_end"),
            ("Editor", "pageup", "editor.page_up"),
            ("Editor", "pagedown", "editor.page_down"),
            ("Editor", "shift+pageup", "editor.extend_page_up"),
            ("Editor", "shift+pagedown", "editor.extend_page_down"),
            ("Editor", "shift shift", "file_picker.open"),
            ("Editor", "cmd+shift+f", "grep.open"),

            // File picker shortcuts
            ("FilePicker", "escape", "file_picker.close"),
            ("FilePicker", "enter", "file_picker.select"),
            ("FilePicker", "up", "file_picker.move_up"),
            ("FilePicker", "down", "file_picker.move_down"),
            ("FilePicker", "backspace", "file_picker.backspace"),

            // Grep shortcuts
            ("Grep", "escape", "grep.close"),
            ("Grep", "enter", "grep.select"),
            ("Grep", "up", "grep.move_up"),
            ("Grep", "down", "grep.move_down"),
            ("Grep", "backspace", "grep.backspace"),
        ];

        for &(ctx_str, accel, event) in SHORTCUTS {
            let context = match ctx_str {
                "Global" => Global,
                "Editor" => Editor,
                "FilePicker" => FilePicker,
                "Grep" => Grep,
                _ => continue,
            };
            self.register(context, accel, event);
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
        let mut registry = ShortcutRegistry::new();
        registry.register(ShortcutContext::Editor, "cmd+k", "test.event");

        let mut mods = Modifiers::default();
        mods.cmd = true;
        let events = registry.match_input(&mods, &Trigger::Char("k".to_string()));

        assert_eq!(events, vec!["test.event".to_string()]);
    }

    #[test]
    fn test_context_switching() {
        let mut registry = ShortcutRegistry::new();
        registry.register(ShortcutContext::Editor, "enter", "editor.enter");
        registry.register(ShortcutContext::FilePicker, "enter", "picker.select");

        // In editor context
        registry.set_context(ShortcutContext::Editor);
        let events =
            registry.match_input(&Modifiers::default(), &Trigger::Named("Enter".to_string()));
        assert_eq!(events, vec!["editor.insert_newline".to_string()]); // From defaults

        // In file picker context
        registry.set_context(ShortcutContext::FilePicker);
        let events =
            registry.match_input(&Modifiers::default(), &Trigger::Named("Enter".to_string()));
        assert_eq!(events, vec!["file_picker.select".to_string()]);
    }
}
