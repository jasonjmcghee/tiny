//! Text renderer for GPU-based text rendering

use crate::{layout::LayoutCache, TextEditorConfig};
use tiny_sdk::{
    bytemuck, ffi::BufferId, GlyphInstance, LayoutPos, PaintContext, PluginError,
    SetupContext, services::{FontService, TextStyleService}, wgpu,
};

/// Text renderer manages GPU resources and rendering
pub struct TextRenderer {
    // GPU buffers for text rendering
    vertex_buffer_id: Option<BufferId>,

    // Cached buffer size
    last_buffer_size: usize,

    // Font atlas texture (if managing our own)
    font_texture: Option<wgpu::Texture>,
}

impl TextRenderer {
    pub fn new() -> Self {
        Self {
            vertex_buffer_id: None,
            last_buffer_size: 0,
            font_texture: None,
        }
    }

    /// Initialize GPU resources
    pub fn setup(&mut self, ctx: &mut SetupContext) -> Result<(), PluginError> {
        // Create initial vertex buffer for glyph instances
        let initial_size = 10000 * std::mem::size_of::<GlyphInstance>();
        let buffer_id = BufferId::create(
            initial_size as u64,
            wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        );

        self.vertex_buffer_id = Some(buffer_id);
        self.last_buffer_size = initial_size;

        Ok(())
    }

    /// Clean up GPU resources
    pub fn cleanup(&mut self) {
        // Cleanup will be handled by dropping
        self.vertex_buffer_id = None;
    }

    /// Paint text using the layout cache
    pub fn paint_text(
        &self,
        ctx: &PaintContext,
        render_pass: &mut wgpu::RenderPass,
        text: &str,
        layout: &LayoutCache,
        config: &TextEditorConfig,
    ) {
        // Get font service from context
        let services = unsafe { ctx.services() };

        // Get the font system - it's registered as SharedFontSystem
        let font_service = services.get::<tiny_font::SharedFontSystem>();
        if font_service.is_none() {
            eprintln!("TextEditor: No font service available");
            return;
        }

        // Get visible glyphs
        let visible_glyphs = layout.get_visible_glyphs(
            ctx.viewport.scroll.y.0,
            ctx.viewport.logical_size.height.0,
        );

        if visible_glyphs.is_empty() {
            return;
        }

        // Convert to GlyphInstances for GPU rendering
        let mut glyph_instances = Vec::new();

        for glyph_info in visible_glyphs {
            // Skip invisible characters
            if glyph_info.char == '\n' || glyph_info.char == '\r' {
                continue;
            }

            // Transform to physical coordinates for GPU
            let physical_pos = ctx.viewport.layout_to_physical(glyph_info.layout_pos);

            glyph_instances.push(GlyphInstance {
                pos: LayoutPos::new(physical_pos.x.0, physical_pos.y.0),
                tex_coords: glyph_info.tex_coords,
                token_id: glyph_info.token_id as u8,
                relative_pos: 0.0,  // Can be used for animation
                shader_id: None,     // Can be used for effects
            });
        }

        if glyph_instances.is_empty() {
            return;
        }

        // Upload glyph data to GPU buffer
        if let Some(buffer_id) = self.vertex_buffer_id {
            let data_size = glyph_instances.len() * std::mem::size_of::<GlyphInstance>();

            // Check if buffer needs to be resized
            if data_size > self.last_buffer_size {
                // In a real implementation, we'd resize the buffer here
                eprintln!("TextEditor: Buffer resize needed");
                return;
            }

            // Write glyph data to buffer
            // GlyphInstance isn't Pod due to Option<u32>, so we need to serialize manually
            let mut buffer_data = Vec::new();
            for glyph in &glyph_instances {
                // Layout pos (2 floats)
                buffer_data.extend_from_slice(&glyph.pos.x.0.to_le_bytes());
                buffer_data.extend_from_slice(&glyph.pos.y.0.to_le_bytes());
                // Tex coords (4 floats)
                for coord in &glyph.tex_coords {
                    buffer_data.extend_from_slice(&coord.to_le_bytes());
                }
                // Token ID (1 byte padded to 4)
                buffer_data.push(glyph.token_id);
                buffer_data.push(0);
                buffer_data.push(0);
                buffer_data.push(0);
                // Relative pos (1 float)
                buffer_data.extend_from_slice(&glyph.relative_pos.to_le_bytes());
                // Shader ID (1 u32)
                let shader_id = glyph.shader_id.unwrap_or(0);
                buffer_data.extend_from_slice(&shader_id.to_le_bytes());
            }
            buffer_id.write(0, &buffer_data);

            // Use GPU context to render
            if let Some(ref gpu_ctx) = ctx.gpu_context {
                // Draw glyphs using the host's text rendering pipeline
                gpu_ctx.draw_vertices(render_pass, buffer_id, glyph_instances.len() as u32);
            } else {
                eprintln!("TextEditor: No GPU context available");
            }
        }
    }

    /// Paint with syntax highlighting support
    pub fn paint_text_with_syntax(
        &self,
        ctx: &PaintContext,
        render_pass: &mut wgpu::RenderPass,
        text: &str,
        layout: &LayoutCache,
        config: &TextEditorConfig,
    ) {
        // Get text style service for syntax highlighting
        // Note: The host registers this as BoxedTextStyleAdapter, but we can't access it from plugin
        // TODO: Need proper interface for text styles from plugins

        // Render with base paint_text
        self.paint_text(ctx, render_pass, text, layout, config);
    }

    /// Update font atlas texture if needed
    pub fn update_font_texture(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        atlas_data: &[u8],
        width: u32,
        height: u32,
    ) {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Font Atlas"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            atlas_data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * 4),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );

        self.font_texture = Some(texture);
    }
}