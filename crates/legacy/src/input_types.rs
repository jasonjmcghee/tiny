//! Input event types abstracted from winit

/// State of a key or button
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElementState {
    Pressed,
    Released,
}

/// Mouse button types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    Other(u16),
}

/// Named keyboard keys
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NamedKey {
    Backspace,
    Delete,
    Enter,
    Tab,
    Space,
    ArrowLeft,
    ArrowRight,
    ArrowUp,
    ArrowDown,
    Home,
    End,
    PageUp,
    PageDown,
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
    Escape,
    // Modifier keys
    Shift,
    Control,
    Alt,
    Super,
}

/// Logical key representation
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Key {
    Character(String),
    Named(NamedKey),
}

/// Keyboard event
#[derive(Debug, Clone)]
pub struct KeyEvent {
    pub state: ElementState,
    pub logical_key: Key,
}

/// Modifier key states
#[derive(Debug, Clone, Copy, Default)]
pub struct ModifierState {
    pub shift: bool,
    pub control: bool,
    pub alt: bool,
    pub super_key: bool, // Command on macOS, Windows key on Windows
}

/// Modifiers wrapper
#[derive(Debug, Clone, Copy, Default)]
pub struct Modifiers {
    state: ModifierState,
}

impl Modifiers {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_state(state: ModifierState) -> Self {
        Self { state }
    }

    pub fn state(&self) -> ModifierState {
        self.state
    }
}

impl ModifierState {
    pub fn shift_key(&self) -> bool {
        self.shift
    }

    pub fn control_key(&self) -> bool {
        self.control
    }

    pub fn alt_key(&self) -> bool {
        self.alt
    }

    pub fn super_key(&self) -> bool {
        self.super_key
    }
}

/// Conversion from winit types
#[cfg(feature = "winit")]
pub mod winit_conversions {
    use super::*;

    impl From<winit::event::ElementState> for ElementState {
        fn from(state: winit::event::ElementState) -> Self {
            match state {
                winit::event::ElementState::Pressed => ElementState::Pressed,
                winit::event::ElementState::Released => ElementState::Released,
            }
        }
    }

    impl From<winit::event::MouseButton> for MouseButton {
        fn from(button: winit::event::MouseButton) -> Self {
            match button {
                winit::event::MouseButton::Left => MouseButton::Left,
                winit::event::MouseButton::Right => MouseButton::Right,
                winit::event::MouseButton::Middle => MouseButton::Middle,
                winit::event::MouseButton::Back => MouseButton::Other(4),
                winit::event::MouseButton::Forward => MouseButton::Other(5),
                winit::event::MouseButton::Other(n) => MouseButton::Other(n),
            }
        }
    }

    impl From<winit::keyboard::NamedKey> for NamedKey {
        fn from(key: winit::keyboard::NamedKey) -> Self {
            match key {
                winit::keyboard::NamedKey::Backspace => NamedKey::Backspace,
                winit::keyboard::NamedKey::Delete => NamedKey::Delete,
                winit::keyboard::NamedKey::Enter => NamedKey::Enter,
                winit::keyboard::NamedKey::Tab => NamedKey::Tab,
                winit::keyboard::NamedKey::Space => NamedKey::Space,
                winit::keyboard::NamedKey::ArrowLeft => NamedKey::ArrowLeft,
                winit::keyboard::NamedKey::ArrowRight => NamedKey::ArrowRight,
                winit::keyboard::NamedKey::ArrowUp => NamedKey::ArrowUp,
                winit::keyboard::NamedKey::ArrowDown => NamedKey::ArrowDown,
                winit::keyboard::NamedKey::Home => NamedKey::Home,
                winit::keyboard::NamedKey::End => NamedKey::End,
                winit::keyboard::NamedKey::PageUp => NamedKey::PageUp,
                winit::keyboard::NamedKey::PageDown => NamedKey::PageDown,
                winit::keyboard::NamedKey::F1 => NamedKey::F1,
                winit::keyboard::NamedKey::F2 => NamedKey::F2,
                winit::keyboard::NamedKey::F3 => NamedKey::F3,
                winit::keyboard::NamedKey::F4 => NamedKey::F4,
                winit::keyboard::NamedKey::F5 => NamedKey::F5,
                winit::keyboard::NamedKey::F6 => NamedKey::F6,
                winit::keyboard::NamedKey::F7 => NamedKey::F7,
                winit::keyboard::NamedKey::F8 => NamedKey::F8,
                winit::keyboard::NamedKey::F9 => NamedKey::F9,
                winit::keyboard::NamedKey::F10 => NamedKey::F10,
                winit::keyboard::NamedKey::F11 => NamedKey::F11,
                winit::keyboard::NamedKey::F12 => NamedKey::F12,
                winit::keyboard::NamedKey::Escape => NamedKey::Escape,
                // Modifier keys
                winit::keyboard::NamedKey::Shift => NamedKey::Shift,
                winit::keyboard::NamedKey::Control => NamedKey::Control,
                winit::keyboard::NamedKey::Alt => NamedKey::Alt,
                winit::keyboard::NamedKey::Super => NamedKey::Super,
                _ => {
                    // Log unhandled keys for debugging
                    eprintln!("Unhandled named key: {:?}", key);
                    return NamedKey::Escape; // Safe fallback that won't type anything
                }
            }
        }
    }

    impl From<&winit::keyboard::Key> for Key {
        fn from(key: &winit::keyboard::Key) -> Self {
            match key {
                winit::keyboard::Key::Character(s) => Key::Character(s.to_string()),
                winit::keyboard::Key::Named(n) => Key::Named(n.clone().into()),
                _ => Key::Character(String::new()), // Fallback for unhandled key types
            }
        }
    }

    impl From<&winit::event::KeyEvent> for KeyEvent {
        fn from(event: &winit::event::KeyEvent) -> Self {
            KeyEvent {
                state: event.state.into(),
                logical_key: (&event.logical_key).into(),
            }
        }
    }

    impl From<&winit::event::Modifiers> for Modifiers {
        fn from(modifiers: &winit::event::Modifiers) -> Self {
            let state = modifiers.state();
            Modifiers::with_state(ModifierState {
                shift: state.shift_key(),
                control: state.control_key(),
                alt: state.alt_key(),
                super_key: state.super_key(),
            })
        }
    }
}