//! Winit adapter - converts winit events to accelerators

use crate::accelerator::{Modifiers, Trigger};
use crate::input::EventBus;
use crate::shortcuts::ShortcutRegistry;
use serde_json::json;
use winit::event::{
    ElementState, Modifiers as WinitModifiers, MouseButton as WinitMouseButton, WindowEvent,
};
use winit::keyboard::{Key, NamedKey};

/// Convert winit modifiers to our Modifiers
pub fn convert_modifiers(winit_mods: &WinitModifiers) -> Modifiers {
    let state = winit_mods.state();
    Modifiers {
        cmd: state.super_key(),
        ctrl: state.control_key(),
        alt: state.alt_key(),
        shift: state.shift_key(),
    }
}

/// Convert winit key to our Trigger
pub fn convert_key(key: &Key) -> Option<Trigger> {
    match key {
        Key::Character(ch) => {
            // Only single character keys
            if ch.len() == 1 {
                Some(Trigger::Char(ch.to_lowercase()))
            } else {
                None
            }
        }
        Key::Named(named) => {
            let name = match named {
                NamedKey::Enter => "Enter",
                NamedKey::Tab => "Tab",
                NamedKey::Backspace => "Backspace",
                NamedKey::Delete => "Delete",
                NamedKey::Escape => "Escape",
                NamedKey::ArrowUp => "ArrowUp",
                NamedKey::ArrowDown => "ArrowDown",
                NamedKey::ArrowLeft => "ArrowLeft",
                NamedKey::ArrowRight => "ArrowRight",
                NamedKey::Home => "Home",
                NamedKey::End => "End",
                NamedKey::PageUp => "PageUp",
                NamedKey::PageDown => "PageDown",
                NamedKey::Space => "Space",
                NamedKey::F12 => "F12",
                NamedKey::Shift => "Shift",
                NamedKey::Control => "Ctrl",
                NamedKey::Alt => "Alt",
                NamedKey::Super => "Cmd",
                _ => return None,
            };
            Some(Trigger::Named(name.to_string()))
        }
        _ => None,
    }
}

/// Handle a winit WindowEvent and emit appropriate events
/// Returns true if the event was handled
pub fn handle_window_event(
    event: &WindowEvent,
    shortcuts: &mut ShortcutRegistry,
    bus: &mut EventBus,
    current_modifiers: &Modifiers,
) -> bool {
    match event {
        WindowEvent::KeyboardInput {
            event: key_event, ..
        } => {
            // Only handle key presses
            if key_event.state != ElementState::Pressed {
                return false;
            }

            // Get the original character for insertion (preserves case and symbols)
            let original_char = if let Key::Character(ch) = &key_event.logical_key {
                if ch.len() == 1 {
                    Some(ch.as_str())
                } else {
                    None
                }
            } else {
                None
            };

            // Convert key to trigger (normalized for shortcut matching)
            if let Some(trigger) = convert_key(&key_event.logical_key) {
                // Try to match shortcut
                let events = shortcuts.match_input(current_modifiers, &trigger);

                // If matched, emit events
                if !events.is_empty() {
                    for event_name in events {
                        bus.emit(event_name, json!({}), 10, "shortcuts");
                    }
                    return true;
                }

                // If no shortcut matched, check for plain character insertion
                if let Some(original) = original_char {
                    if !current_modifiers.cmd
                        && !current_modifiers.ctrl
                        && !current_modifiers.alt
                        && !original.chars().any(|c| c.is_control())
                    {
                        // Plain character - emit insert event with ORIGINAL character (preserves shift)
                        bus.emit(
                            "editor.insert_char",
                            json!({ "char": original }),
                            10,
                            "winit",
                        );
                        return true;
                    }
                }
            }

            false
        }

        WindowEvent::MouseInput { state, button, .. } => {
            if *button == WinitMouseButton::Left && *state == ElementState::Pressed {
                // Emit mouse press event (will be handled separately)
                // This is just for detection, actual position comes from CursorMoved
                true
            } else {
                false
            }
        }

        _ => false,
    }
}
