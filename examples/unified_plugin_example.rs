// #!/usr/bin/env cargo +nightly -Zscript

//! Example implementation of the unified plugin architecture
//! Shows how widgets, text effects, and GPU pipelines all compose

fn main() {}

// use std::any::Any;
// use std::collections::HashMap;
// use std::sync::{Arc, RwLock};
//
// // ============================================================================
// // Core Transform Abstraction
// // ============================================================================
//
// pub trait Transform: Send + Sync {
//     fn transform(&self, input: &dyn Any) -> Box<dyn Any>;
//
//     fn capabilities(&self) -> Capabilities {
//         Capabilities::default()
//     }
//
//     fn compose(self: Box<Self>, next: Box<dyn Transform>) -> Box<dyn Transform>
//     where
//         Self: 'static
//     {
//         Box::new(ComposedTransform {
//             first: self,
//             second: next,
//         })
//     }
// }
//
// struct ComposedTransform {
//     first: Box<dyn Transform>,
//     second: Box<dyn Transform>,
// }
//
// impl Transform for ComposedTransform {
//     fn transform(&self, input: &dyn Any) -> Box<dyn Any> {
//         let intermediate = self.first.transform(input);
//         self.second.transform(intermediate.as_ref())
//     }
// }
//
// // ============================================================================
// // Capability System
// // ============================================================================
//
// #[derive(Default, Clone)]
// pub struct Capabilities {
//     provides: Vec<String>,
//     requires: Vec<String>,
// }
//
// impl Capabilities {
//     pub fn provides(mut self, cap: &str) -> Self {
//         self.provides.push(cap.to_string());
//         self
//     }
//
//     pub fn requires(mut self, cap: &str) -> Self {
//         self.requires.push(cap.to_string());
//         self
//     }
// }
//
// // ============================================================================
// // Port System for Data Flow
// // ============================================================================
//
// pub struct Port {
//     id: String,
//     data: Arc<RwLock<Box<dyn Any>>>,
//     subscribers: Vec<Box<dyn Transform>>,
// }
//
// impl Port {
//     pub fn new(id: &str) -> Self {
//         Self {
//             id: id.to_string(),
//             data: Arc::new(RwLock::new(Box::new(()) as Box<dyn Any>)),
//             subscribers: Vec::new(),
//         }
//     }
//
//     pub fn publish(&self, data: Box<dyn Any>) {
//         *self.data.write().unwrap() = data;
//
//         // Transform data through all subscribers
//         for subscriber in &self.subscribers {
//             let input = self.data.read().unwrap();
//             let _output = subscriber.transform(input.as_ref());
//             // In real impl, output would flow to next port
//         }
//     }
//
//     pub fn subscribe(&mut self, transform: Box<dyn Transform>) {
//         self.subscribers.push(transform);
//     }
// }
//
// // ============================================================================
// // Example: Text Editor Widget as Transform
// // ============================================================================
//
// pub struct TextEditorWidget {
//     content: String,
//     cursor: usize,
// }
//
// impl TextEditorWidget {
//     pub fn new() -> Self {
//         Self {
//             content: String::new(),
//             cursor: 0,
//         }
//     }
// }
//
// impl Transform for TextEditorWidget {
//     fn transform(&self, input: &dyn Any) -> Box<dyn Any> {
//         // Handle different input types
//         if let Some(text) = input.downcast_ref::<String>() {
//             // Transform text input into document update
//             Box::new(DocumentUpdate {
//                 content: text.clone(),
//                 cursor: self.cursor,
//             })
//         } else if let Some(cmd) = input.downcast_ref::<EditorCommand>() {
//             // Handle editor commands
//             match cmd {
//                 EditorCommand::Insert(ch) => {
//                     let mut new_content = self.content.clone();
//                     new_content.insert(self.cursor, *ch);
//                     Box::new(DocumentUpdate {
//                         content: new_content,
//                         cursor: self.cursor + 1,
//                     })
//                 }
//                 EditorCommand::Delete => {
//                     let mut new_content = self.content.clone();
//                     if self.cursor > 0 {
//                         new_content.remove(self.cursor - 1);
//                     }
//                     Box::new(DocumentUpdate {
//                         content: new_content,
//                         cursor: self.cursor.saturating_sub(1),
//                     })
//                 }
//             }
//         } else {
//             Box::new(())
//         }
//     }
//
//     fn capabilities(&self) -> Capabilities {
//         Capabilities::default()
//             .provides("text.content")
//             .provides("text.editing")
//             .requires("font.rendering")
//     }
// }
//
// // ============================================================================
// // Example: Syntax Highlighter as Transform
// // ============================================================================
//
// pub struct SyntaxHighlighter {
//     language: String,
// }
//
// impl Transform for SyntaxHighlighter {
//     fn transform(&self, input: &dyn Any) -> Box<dyn Any> {
//         if let Some(update) = input.downcast_ref::<DocumentUpdate>() {
//             // Parse text and generate tokens
//             let tokens = self.parse(&update.content);
//             Box::new(SyntaxTokens { tokens })
//         } else {
//             Box::new(())
//         }
//     }
//
//     fn capabilities(&self) -> Capabilities {
//         Capabilities::default()
//             .provides("syntax.highlighting")
//             .requires("text.content")
//     }
// }
//
// impl SyntaxHighlighter {
//     fn parse(&self, text: &str) -> Vec<Token> {
//         // Simplified parsing
//         text.split_whitespace()
//             .enumerate()
//             .map(|(i, word)| Token {
//                 text: word.to_string(),
//                 kind: if i % 2 == 0 {
//                     TokenKind::Keyword
//                 } else {
//                     TokenKind::Identifier
//                 },
//                 range: 0..word.len(),
//             })
//             .collect()
//     }
// }
//
// // ============================================================================
// // Example: GPU Pipeline as Transform
// // ============================================================================
//
// pub struct RainbowGlowPipeline {
//     time: f32,
// }
//
// impl Transform for RainbowGlowPipeline {
//     fn transform(&self, input: &dyn Any) -> Box<dyn Any> {
//         if let Some(tokens) = input.downcast_ref::<SyntaxTokens>() {
//             // Transform tokens into GPU commands
//             let commands: Vec<GpuCommand> = tokens.tokens.iter()
//                 .map(|token| {
//                     let color = match token.kind {
//                         TokenKind::Keyword => [1.0, 0.0, 0.0, 1.0],
//                         TokenKind::Identifier => [0.0, 1.0, 0.0, 1.0],
//                     };
//                     GpuCommand::DrawText {
//                         text: token.text.clone(),
//                         color,
//                         glow: (self.time * 2.0).sin() * 0.5 + 0.5,
//                     }
//                 })
//                 .collect();
//
//             Box::new(GpuCommands { commands })
//         } else {
//             Box::new(())
//         }
//     }
//
//     fn capabilities(&self) -> Capabilities {
//         Capabilities::default()
//             .provides("gpu.pipeline.rainbow_glow")
//             .requires("syntax.highlighting")
//     }
// }
//
// // ============================================================================
// // Example: Physics Simulation as Transform
// // ============================================================================
//
// pub struct FallingSandPhysics {
//     gravity: f32,
//     particles: Vec<Particle>,
// }
//
// impl Transform for FallingSandPhysics {
//     fn transform(&self, input: &dyn Any) -> Box<dyn Any> {
//         if let Some(commands) = input.downcast_ref::<GpuCommands>() {
//             // Apply physics to each glyph
//             let mut physics_commands = commands.commands.clone();
//             for (i, cmd) in physics_commands.iter_mut().enumerate() {
//                 if let GpuCommand::DrawText { glow, .. } = cmd {
//                     // Simulate falling based on glow intensity
//                     *glow += self.gravity * (i as f32 * 0.1);
//                 }
//             }
//             Box::new(GpuCommands { commands: physics_commands })
//         } else {
//             Box::new(())
//         }
//     }
//
//     fn capabilities(&self) -> Capabilities {
//         Capabilities::default()
//             .provides("physics.falling_sand")
//             .requires("gpu.pipeline")
//     }
// }
//
// // ============================================================================
// // Plugin System
// // ============================================================================
//
// pub struct PluginRegistry {
//     transforms: HashMap<String, Box<dyn Transform>>,
//     capabilities: HashMap<String, Vec<String>>, // capability -> providers
// }
//
// impl PluginRegistry {
//     pub fn new() -> Self {
//         Self {
//             transforms: HashMap::new(),
//             capabilities: HashMap::new(),
//         }
//     }
//
//     pub fn register(&mut self, name: &str, transform: Box<dyn Transform>) {
//         let caps = transform.capabilities();
//
//         // Register capabilities
//         for cap in &caps.provides {
//             self.capabilities.entry(cap.clone())
//                 .or_default()
//                 .push(name.to_string());
//         }
//
//         self.transforms.insert(name.to_string(), transform);
//     }
//
//     pub fn build_pipeline(&self, requirements: &[&str]) -> Option<Box<dyn Transform>> {
//         // Find transforms that satisfy requirements
//         let mut pipeline: Option<Box<dyn Transform>> = None;
//
//         for req in requirements {
//             if let Some(providers) = self.capabilities.get(*req) {
//                 if let Some(provider_name) = providers.first() {
//                     if let Some(transform) = self.transforms.get(provider_name) {
//                         // Clone isn't available on trait objects, so this is simplified
//                         // In real implementation, we'd use Arc or factory pattern
//                         println!("Adding {} to pipeline for capability {}", provider_name, req);
//                     }
//                 }
//             }
//         }
//
//         pipeline
//     }
// }
//
// // ============================================================================
// // Data Types
// // ============================================================================
//
// #[derive(Clone)]
// pub struct DocumentUpdate {
//     content: String,
//     cursor: usize,
// }
//
// pub enum EditorCommand {
//     Insert(char),
//     Delete,
// }
//
// #[derive(Clone)]
// pub struct Token {
//     text: String,
//     kind: TokenKind,
//     range: std::ops::Range<usize>,
// }
//
// #[derive(Clone)]
// pub enum TokenKind {
//     Keyword,
//     Identifier,
// }
//
// pub struct SyntaxTokens {
//     tokens: Vec<Token>,
// }
//
// #[derive(Clone)]
// pub enum GpuCommand {
//     DrawText {
//         text: String,
//         color: [f32; 4],
//         glow: f32,
//     },
// }
//
// pub struct GpuCommands {
//     commands: Vec<GpuCommand>,
// }
//
// pub struct Particle {
//     pos: [f32; 2],
//     vel: [f32; 2],
// }
//
// // ============================================================================
// // Example Usage
// // ============================================================================
//
// fn main() {
//     println!("🚀 Unified Plugin Architecture Demo\n");
//
//     // Create plugin registry
//     let mut registry = PluginRegistry::new();
//
//     // Register Alice's text editor
//     println!("📝 Alice registers text editor...");
//     registry.register("alice.editor", Box::new(TextEditorWidget::new()));
//
//     // Register Bob's syntax highlighter
//     println!("🎨 Bob registers syntax highlighter...");
//     registry.register("bob.syntax", Box::new(SyntaxHighlighter {
//         language: "rust".to_string(),
//     }));
//
//     // Register Charlie's GPU pipeline
//     println!("🌈 Charlie registers rainbow glow pipeline...");
//     registry.register("charlie.rainbow", Box::new(RainbowGlowPipeline {
//         time: 0.0,
//     }));
//
//     // Register Dave's physics
//     println!("⚛️ Dave registers falling sand physics...");
//     registry.register("dave.physics", Box::new(FallingSandPhysics {
//         gravity: 9.8,
//         particles: vec![],
//     }));
//
//     // System automatically builds pipeline
//     println!("\n🔗 System building execution pipeline...");
//     let requirements = [
//         "text.content",
//         "syntax.highlighting",
//         "gpu.pipeline.rainbow_glow",
//         "physics.falling_sand"
//     ];
//
//     if let Some(_pipeline) = registry.build_pipeline(&requirements) {
//         println!("✅ Pipeline built successfully!");
//
//         // In real implementation, this would:
//         // 1. Wire up ports between transforms
//         // 2. Set up hot reload watchers
//         // 3. Start event loop
//         // 4. Handle composition automatically
//     }
//
//     println!("\n🎯 The magic: All plugins work together automatically!");
//     println!("   - Alice's editor provides text");
//     println!("   - Bob's highlighter adds syntax coloring");
//     println!("   - Charlie's pipeline adds visual effects");
//     println!("   - Dave's physics makes it interactive");
//     println!("\n✨ True substrate software - infinitely extensible!");
// }
//
// #[cfg(test)]
// mod tests {
//     use super::*;
//
//     #[test]
//     fn test_transform_composition() {
//         let editor = Box::new(TextEditorWidget::new());
//         let highlighter = Box::new(SyntaxHighlighter {
//             language: "rust".to_string(),
//         });
//
//         let composed = editor.compose(highlighter);
//
//         // Test that composition preserves transform behavior
//         let input = String::from("fn main() {}");
//         let output = composed.transform(&input as &dyn Any);
//         assert!(output.downcast_ref::<SyntaxTokens>().is_some());
//     }
//
//     #[test]
//     fn test_capability_matching() {
//         let transform = TextEditorWidget::new();
//         let caps = transform.capabilities();
//
//         assert!(caps.provides.contains(&"text.content".to_string()));
//         assert!(caps.requires.contains(&"font.rendering".to_string()));
//     }
//
//     #[test]
//     fn test_port_pubsub() {
//         let mut port = Port::new("test.port");
//         let subscriber = Box::new(SyntaxHighlighter {
//             language: "rust".to_string(),
//         });
//
//         port.subscribe(subscriber);
//         port.publish(Box::new(DocumentUpdate {
//             content: "test".to_string(),
//             cursor: 0,
//         }));
//
//         // In real impl, would verify transform was called
//     }
// }
