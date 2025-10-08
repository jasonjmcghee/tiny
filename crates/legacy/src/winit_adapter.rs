//! Winit adapter - converts winit events to internal semantic events

use crate::accelerator::{Modifiers, Trigger};
use crate::input::EventBus;
use crate::mouse_state::MouseState;
use crate::shortcuts::ShortcutRegistry;
use serde_json::json;
use winit::dpi::PhysicalPosition;
use winit::event::{
    ElementState, MouseScrollDelta, Modifiers as WinitModifiers, MouseButton as WinitMouseButton,
    WindowEvent,
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

/// Handle a winit WindowEvent and emit appropriate semantic events
/// Returns true if the event was handled and should not propagate further
pub fn handle_window_event(
    event: &WindowEvent,
    shortcuts: &mut ShortcutRegistry,
    bus: &mut EventBus,
    mouse_state: &mut MouseState,
) -> bool {
    match event {
        WindowEvent::ModifiersChanged(winit_mods) => {
            // Update mouse state with new modifiers
            mouse_state.set_modifiers(convert_modifiers(winit_mods));
            true
        }

        WindowEvent::CursorMoved { position, .. } => {
            // Update mouse state and emit event
            mouse_state.set_position(*position);
            bus.emit(
                "mouse.moved",
                json!({
                    "x": position.x,
                    "y": position.y,
                }),
                10,
                "winit",
            );
            true
        }

        WindowEvent::MouseInput { state, button, .. } => {
            let event_name = match (button, state) {
                (WinitMouseButton::Left, ElementState::Pressed) => {
                    if let Some(pos) = mouse_state.position {
                        mouse_state.start_drag(pos);
                    }
                    "mouse.press"
                }
                (WinitMouseButton::Left, ElementState::Released) => {
                    mouse_state.end_drag();
                    "mouse.release"
                }
                (WinitMouseButton::Right, ElementState::Pressed) => "mouse.right_press",
                (WinitMouseButton::Right, ElementState::Released) => "mouse.right_release",
                _ => return false,
            };

            // Check for shortcuts first (e.g., cmd+click)
            let trigger = match button {
                WinitMouseButton::Left => Trigger::MouseButton(crate::accelerator::MouseButton::Left),
                WinitMouseButton::Right => Trigger::MouseButton(crate::accelerator::MouseButton::Right),
                _ => return false,
            };

            let shortcut_events = shortcuts.match_input(&mouse_state.modifiers, &trigger);
            if !shortcut_events.is_empty() {
                for event in shortcut_events {
                    bus.emit(event, json!({}), 10, "shortcuts");
                }
                return true;
            }

            // No shortcut, emit mouse event
            if let Some(position) = mouse_state.position {
                bus.emit(
                    event_name,
                    json!({
                        "x": position.x,
                        "y": position.y,
                        "modifiers": {
                            "cmd": mouse_state.modifiers.cmd,
                            "ctrl": mouse_state.modifiers.ctrl,
                            "alt": mouse_state.modifiers.alt,
                            "shift": mouse_state.modifiers.shift,
                        }
                    }),
                    10,
                    "winit",
                );
            }
            true
        }

        WindowEvent::MouseWheel { delta, .. } => {
            let (delta_x, delta_y) = match delta {
                MouseScrollDelta::LineDelta(x, y) => (*x as f64 * 20.0, *y as f64 * 20.0),
                MouseScrollDelta::PixelDelta(pos) => (pos.x, pos.y),
            };

            bus.emit(
                "mouse.wheel",
                json!({
                    "delta_x": delta_x,
                    "delta_y": delta_y,
                    "modifiers": {
                        "cmd": mouse_state.modifiers.cmd,
                        "ctrl": mouse_state.modifiers.ctrl,
                        "alt": mouse_state.modifiers.alt,
                        "shift": mouse_state.modifiers.shift,
                    }
                }),
                10,
                "winit",
            );
            true
        }

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
                let events = shortcuts.match_input(&mouse_state.modifiers, &trigger);

                // If matched, emit events
                if !events.is_empty() {
                    for event_name in events {
                        bus.emit(event_name, json!({}), 10, "shortcuts");
                    }
                    return true;
                }

                // If no shortcut matched, check for plain character insertion
                if let Some(original) = original_char {
                    if !mouse_state.modifiers.cmd
                        && !mouse_state.modifiers.ctrl
                        && !mouse_state.modifiers.alt
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

        _ => false,
    }
}
