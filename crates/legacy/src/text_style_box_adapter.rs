//! Adapter for Box<dyn TextStyleProvider> to work with services

use std::sync::Arc;
use tiny_sdk::services::{TextEffect, TextStyleService};
use crate::text_effects::TextStyleProvider;

/// Wraps a Box<dyn TextStyleProvider> for service registration
pub struct BoxedTextStyleAdapter {
    provider: *const dyn TextStyleProvider,
}

impl BoxedTextStyleAdapter {
    pub fn new(provider: Box<dyn TextStyleProvider>) -> Arc<Self> {
        let ptr = Box::into_raw(provider) as *const dyn TextStyleProvider;
        Arc::new(Self { provider: ptr })
    }

    pub fn from_ref(provider: &Box<dyn TextStyleProvider>) -> Arc<Self> {
        // Store a raw pointer to the provider
        Arc::new(Self {
            provider: provider.as_ref() as *const dyn TextStyleProvider
        })
    }
}

impl TextStyleService for BoxedTextStyleAdapter {
    fn get_effects_in_range(&self, range: std::ops::Range<usize>) -> Vec<TextEffect> {
        let legacy_effects = unsafe { &*self.provider }.get_effects_in_range(range);

        // Convert legacy effects to SDK effects
        legacy_effects
            .into_iter()
            .map(|effect| {
                let sdk_effect = match effect.effect {
                    crate::text_effects::EffectType::Token(token_id) => {
                        tiny_sdk::services::TextEffectType::Token(token_id)
                    }
                    crate::text_effects::EffectType::Shader { id, params } => {
                        tiny_sdk::services::TextEffectType::Shader {
                            id,
                            params: params.map(|p| p.to_vec()),
                        }
                    }
                    _ => {
                        // Other effects not supported yet, default to token 0
                        tiny_sdk::services::TextEffectType::Token(0)
                    }
                };

                TextEffect {
                    range: effect.range,
                    effect: sdk_effect,
                    priority: effect.priority as i32,
                }
            })
            .collect()
    }

    fn request_update(&self, text: &str, version: u64) {
        unsafe { &*self.provider }.request_update(text, version);
    }

    fn name(&self) -> &str {
        unsafe { &*self.provider }.name()
    }
}

// Safe because we only use this in a single-threaded context
unsafe impl Send for BoxedTextStyleAdapter {}
unsafe impl Sync for BoxedTextStyleAdapter {}