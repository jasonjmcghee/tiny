use std::ops::Range;
use tiny_editor::text_effects::*;

struct MockProvider {
    effects: Vec<TextEffect>,
}

impl TextStyleProvider for MockProvider {
    fn get_effects_in_range(&self, range: Range<usize>) -> Vec<TextEffect> {
        self.effects
            .iter()
            .filter(|e| e.range.start < range.end && e.range.end > range.start)
            .cloned()
            .collect()
    }

    fn request_update(&self, _text: &str, _version: u64) {
        // Mock implementation
    }

    fn name(&self) -> &str {
        "mock"
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[test]
fn test_range_filtering() {
    let provider = MockProvider {
        effects: vec![
            TextEffect {
                range: 0..10,
                effect: EffectType::Token(1), // Token ID 1
                priority: priority::SYNTAX,
            },
            TextEffect {
                range: 20..30,
                effect: EffectType::Token(2), // Token ID 2
                priority: priority::SYNTAX,
            },
        ],
    };

    let effects = provider.get_effects_in_range(5..15);
    assert_eq!(effects.len(), 1);
    assert_eq!(effects[0].range, 0..10);
}
