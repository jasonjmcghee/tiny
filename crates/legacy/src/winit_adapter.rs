//! Winit adapter - converts winit events to internal semantic events

use crate::accelerator::{Modifiers, Trigger};
use winit::event::Modifiers as WinitModifiers;
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
