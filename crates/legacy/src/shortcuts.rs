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

    /// Register default shortcuts
    fn register_defaults(&mut self) {
        // Global shortcuts
        self.register(ShortcutContext::Global, "cmd+=", "app.font_increase");
        self.register(ShortcutContext::Global, "cmd+-", "app.font_decrease");
        self.register(ShortcutContext::Global, "f12", "app.toggle_scroll_lock");

        // Editor shortcuts
        self.register(ShortcutContext::Editor, "cmd+s", "editor.save");
        self.register(ShortcutContext::Editor, "cmd+z", "editor.undo");
        self.register(ShortcutContext::Editor, "cmd+shift+z", "editor.redo");
        self.register(ShortcutContext::Editor, "cmd+c", "editor.copy");
        self.register(ShortcutContext::Editor, "cmd+x", "editor.cut");
        self.register(ShortcutContext::Editor, "cmd+v", "editor.paste");
        self.register(ShortcutContext::Editor, "cmd+a", "editor.select_all");
        self.register(
            ShortcutContext::Editor,
            "cmd+b",
            "navigation.goto_definition",
        );
        self.register(ShortcutContext::Editor, "cmd+[", "navigation.back");
        self.register(ShortcutContext::Editor, "cmd+]", "navigation.forward");
        self.register(ShortcutContext::Editor, "cmd+w", "tabs.close");
        self.register(ShortcutContext::Editor, "alt+enter", "editor.code_action");

        // Text editing
        self.register(ShortcutContext::Editor, "enter", "editor.insert_newline");
        self.register(ShortcutContext::Editor, "tab", "editor.insert_tab");
        self.register(ShortcutContext::Editor, "space", "editor.insert_space");
        self.register(
            ShortcutContext::Editor,
            "backspace",
            "editor.delete_backward",
        );
        self.register(ShortcutContext::Editor, "delete", "editor.delete_forward");

        // Navigation
        self.register(ShortcutContext::Editor, "left", "editor.move_left");
        self.register(ShortcutContext::Editor, "right", "editor.move_right");
        self.register(ShortcutContext::Editor, "up", "editor.move_up");
        self.register(ShortcutContext::Editor, "down", "editor.move_down");
        self.register(ShortcutContext::Editor, "shift+left", "editor.extend_left");
        self.register(
            ShortcutContext::Editor,
            "shift+right",
            "editor.extend_right",
        );
        self.register(ShortcutContext::Editor, "shift+up", "editor.extend_up");
        self.register(ShortcutContext::Editor, "shift+down", "editor.extend_down");
        self.register(ShortcutContext::Editor, "home", "editor.move_line_start");
        self.register(ShortcutContext::Editor, "end", "editor.move_line_end");
        self.register(
            ShortcutContext::Editor,
            "shift+home",
            "editor.extend_line_start",
        );
        self.register(
            ShortcutContext::Editor,
            "shift+end",
            "editor.extend_line_end",
        );
        self.register(ShortcutContext::Editor, "pageup", "editor.page_up");
        self.register(ShortcutContext::Editor, "pagedown", "editor.page_down");
        self.register(
            ShortcutContext::Editor,
            "shift+pageup",
            "editor.extend_page_up",
        );
        self.register(
            ShortcutContext::Editor,
            "shift+pagedown",
            "editor.extend_page_down",
        );

        // Double-shift for file picker
        self.register(ShortcutContext::Editor, "shift shift", "file_picker.open");

        // Cmd+Shift+F for grep search
        self.register(ShortcutContext::Editor, "cmd+shift+f", "grep.open");

        // File picker shortcuts
        self.register(ShortcutContext::FilePicker, "escape", "file_picker.close");
        self.register(ShortcutContext::FilePicker, "enter", "file_picker.select");
        self.register(ShortcutContext::FilePicker, "up", "file_picker.move_up");
        self.register(ShortcutContext::FilePicker, "down", "file_picker.move_down");
        self.register(
            ShortcutContext::FilePicker,
            "backspace",
            "file_picker.backspace",
        );

        // Grep shortcuts
        self.register(ShortcutContext::Grep, "escape", "grep.close");
        self.register(ShortcutContext::Grep, "enter", "grep.select");
        self.register(ShortcutContext::Grep, "up", "grep.move_up");
        self.register(ShortcutContext::Grep, "down", "grep.move_down");
        self.register(ShortcutContext::Grep, "backspace", "grep.backspace");
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
