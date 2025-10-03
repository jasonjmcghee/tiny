//! Accelerator parsing and matching
//!
//! Supports standard accelerator syntax:
//! - "cmd+k" - modifier + key
//! - "shift shift" - repeated key (double tap)
//! - "g d" - vim-style sequence
//! - "alt+click" - modifier + mouse action

use std::time::{Duration, Instant};

/// Represents a keyboard/mouse accelerator (may be a sequence)
#[derive(Debug, Clone, PartialEq)]
pub struct Accelerator {
    /// Sequence of chords (e.g., "g d" has two chords)
    pub chords: Vec<Chord>,
}

/// A single chord in an accelerator (modifiers + trigger)
#[derive(Debug, Clone, PartialEq)]
pub struct Chord {
    pub modifiers: Modifiers,
    pub trigger: Trigger,
}

/// Modifier keys
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Modifiers {
    pub cmd: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl Modifiers {
    pub fn none() -> Self {
        Self::default()
    }

    pub fn matches(&self, other: &Modifiers) -> bool {
        self.cmd == other.cmd
            && self.ctrl == other.ctrl
            && self.alt == other.alt
            && self.shift == other.shift
    }
}

/// The actual key or action that triggers
#[derive(Debug, Clone, PartialEq)]
pub enum Trigger {
    /// Character key (a-z, etc)
    Char(String),
    /// Named key (Enter, ArrowUp, etc)
    Named(String),
    /// Mouse button click (Left, Right, Middle)
    MouseButton(MouseButton),
    /// Mouse wheel scroll (Up or Down)
    MouseWheel(WheelDirection),
}

/// Mouse button
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

/// Mouse wheel direction
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WheelDirection {
    Up,
    Down,
    Left,
    Right,
}

impl Accelerator {
    /// Parse an accelerator string
    /// Examples: "cmd+k", "shift shift", "g d", "alt+click"
    pub fn parse(input: &str) -> Result<Self, String> {
        let parts: Vec<&str> = input.split_whitespace().collect();
        if parts.is_empty() {
            return Err("Empty accelerator".to_string());
        }

        let mut chords = Vec::new();
        for part in parts {
            chords.push(Chord::parse(part)?);
        }

        Ok(Accelerator { chords })
    }

    /// Check if this is a single-chord accelerator
    pub fn is_simple(&self) -> bool {
        self.chords.len() == 1
    }
}

impl Chord {
    /// Parse a single chord like "cmd+k" or "shift+alt+s" or just "g"
    pub fn parse(input: &str) -> Result<Self, String> {
        let parts: Vec<&str> = input.split('+').collect();
        if parts.is_empty() {
            return Err("Empty chord".to_string());
        }

        let mut modifiers = Modifiers::default();
        let trigger;

        // Last part is the trigger, rest are modifiers
        let (mod_parts, trigger_part) = parts.split_at(parts.len() - 1);

        for mod_str in mod_parts {
            match mod_str.to_lowercase().as_str() {
                "cmd" | "super" => modifiers.cmd = true,
                "ctrl" | "control" => modifiers.ctrl = true,
                "alt" | "option" => modifiers.alt = true,
                "shift" => modifiers.shift = true,
                _ => return Err(format!("Unknown modifier: {}", mod_str)),
            }
        }

        // Parse trigger
        let trigger_str = trigger_part[0];
        trigger = if trigger_str.len() == 1 && trigger_str.chars().next().unwrap().is_alphabetic() {
            Trigger::Char(trigger_str.to_lowercase())
        } else {
            match trigger_str.to_lowercase().as_str() {
                // Mouse buttons
                "click" | "lclick" | "leftclick" => Trigger::MouseButton(MouseButton::Left),
                "rclick" | "rightclick" => Trigger::MouseButton(MouseButton::Right),
                "mclick" | "middleclick" => Trigger::MouseButton(MouseButton::Middle),
                // Mouse wheel
                "wheelup" => Trigger::MouseWheel(WheelDirection::Up),
                "wheeldown" => Trigger::MouseWheel(WheelDirection::Down),
                "wheelleft" => Trigger::MouseWheel(WheelDirection::Left),
                "wheelright" => Trigger::MouseWheel(WheelDirection::Right),
                // Modifier keys as triggers (for sequences like "shift shift")
                "shift" => Trigger::Named("Shift".to_string()),
                "ctrl" | "control" => Trigger::Named("Ctrl".to_string()),
                "alt" | "option" => Trigger::Named("Alt".to_string()),
                "cmd" | "super" => Trigger::Named("Cmd".to_string()),
                // Named keys
                "enter" | "return" => Trigger::Named("Enter".to_string()),
                "tab" => Trigger::Named("Tab".to_string()),
                "backspace" => Trigger::Named("Backspace".to_string()),
                "delete" => Trigger::Named("Delete".to_string()),
                "escape" | "esc" => Trigger::Named("Escape".to_string()),
                "space" => Trigger::Named("Space".to_string()),
                "up" | "arrowup" => Trigger::Named("ArrowUp".to_string()),
                "down" | "arrowdown" => Trigger::Named("ArrowDown".to_string()),
                "left" | "arrowleft" => Trigger::Named("ArrowLeft".to_string()),
                "right" | "arrowright" => Trigger::Named("ArrowRight".to_string()),
                "home" => Trigger::Named("Home".to_string()),
                "end" => Trigger::Named("End".to_string()),
                "pageup" => Trigger::Named("PageUp".to_string()),
                "pagedown" => Trigger::Named("PageDown".to_string()),
                "f12" => Trigger::Named("F12".to_string()),
                _ => Trigger::Char(trigger_str.to_string()),
            }
        };

        Ok(Chord { modifiers, trigger })
    }

    /// Check if this chord matches the given input
    pub fn matches(&self, modifiers: &Modifiers, trigger: &Trigger) -> bool {
        self.modifiers.matches(modifiers) && &self.trigger == trigger
    }
}

/// Tracks sequence matching state (for multi-chord accelerators)
pub struct AcceleratorMatcher {
    /// Current sequence being built
    current_sequence: Vec<Chord>,
    /// When the last chord was pressed (for timeout)
    last_chord_time: Option<Instant>,
    /// Timeout for sequences (reset if exceeded)
    sequence_timeout: Duration,
}

impl AcceleratorMatcher {
    pub fn new() -> Self {
        Self {
            current_sequence: Vec::new(),
            last_chord_time: None,
            sequence_timeout: Duration::from_millis(1000),
        }
    }

    /// Feed a chord into the matcher and check if any accelerator matches
    /// Returns Some(accelerator) if a complete match is found
    pub fn feed(
        &mut self,
        modifiers: &Modifiers,
        trigger: &Trigger,
        candidates: &[Accelerator],
    ) -> Option<Accelerator> {
        let now = Instant::now();

        // Reset if timeout exceeded
        if let Some(last_time) = self.last_chord_time {
            if now.duration_since(last_time) > self.sequence_timeout {
                self.current_sequence.clear();
            }
        }

        // Add current chord to sequence
        self.current_sequence.push(Chord {
            modifiers: modifiers.clone(),
            trigger: trigger.clone(),
        });
        self.last_chord_time = Some(now);

        // Check for matches
        for accelerator in candidates {
            if self.matches_accelerator(accelerator) {
                let matched = accelerator.clone();
                self.current_sequence.clear();
                return Some(matched);
            }
        }

        // Check if any accelerator could still match with more input
        let could_match = candidates.iter().any(|acc| {
            acc.chords.len() >= self.current_sequence.len()
                && acc.chords[..self.current_sequence.len()] == self.current_sequence[..]
        });

        // If no potential matches, reset
        if !could_match {
            self.current_sequence.clear();
        }

        None
    }

    /// Check if current sequence matches an accelerator
    fn matches_accelerator(&self, accelerator: &Accelerator) -> bool {
        if self.current_sequence.len() != accelerator.chords.len() {
            return false;
        }

        for (current, target) in self.current_sequence.iter().zip(accelerator.chords.iter()) {
            if !current.modifiers.matches(&target.modifiers) || current.trigger != target.trigger {
                return false;
            }
        }

        true
    }

    /// Reset the matcher state
    pub fn reset(&mut self) {
        self.current_sequence.clear();
        self.last_chord_time = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple() {
        let acc = Accelerator::parse("cmd+k").unwrap();
        assert_eq!(acc.chords.len(), 1);
        assert!(acc.chords[0].modifiers.cmd);
        assert_eq!(acc.chords[0].trigger, Trigger::Char("k".to_string()));
    }

    #[test]
    fn test_parse_sequence() {
        let acc = Accelerator::parse("g d").unwrap();
        assert_eq!(acc.chords.len(), 2);
        assert_eq!(acc.chords[0].trigger, Trigger::Char("g".to_string()));
        assert_eq!(acc.chords[1].trigger, Trigger::Char("d".to_string()));
    }

    #[test]
    fn test_parse_repeated() {
        let acc = Accelerator::parse("shift shift").unwrap();
        assert_eq!(acc.chords.len(), 2);
        assert!(acc.chords[0].modifiers.shift);
        assert!(acc.chords[1].modifiers.shift);
    }

    #[test]
    fn test_matcher_simple() {
        let mut matcher = AcceleratorMatcher::new();
        let candidates = vec![Accelerator::parse("cmd+k").unwrap()];

        let mut mods = Modifiers::none();
        mods.cmd = true;
        let result = matcher.feed(&mods, &Trigger::Char("k".to_string()), &candidates);

        assert!(result.is_some());
    }

    #[test]
    fn test_matcher_sequence() {
        let mut matcher = AcceleratorMatcher::new();
        let candidates = vec![Accelerator::parse("g d").unwrap()];

        // First chord
        let result = matcher.feed(
            &Modifiers::none(),
            &Trigger::Char("g".to_string()),
            &candidates,
        );
        assert!(result.is_none());

        // Second chord - should match
        let result = matcher.feed(
            &Modifiers::none(),
            &Trigger::Char("d".to_string()),
            &candidates,
        );
        assert!(result.is_some());
    }
}
