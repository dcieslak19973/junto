//! GPU rendering of a browser screencast frame into a **persistent texture**.
//!
//! The obvious path — a new `image::Handle` per frame — strobes: iced stamps
//! every frame with a unique id, so its wgpu atlas allocates a texture and
//! trims the previous one *every frame* (`iced_wgpu`'s `raster::Cache::trim`),
//! and that per-frame allocate/free churn is the flicker. The fix is to own one
//! `wgpu::Texture` and rewrite it in place each frame via a custom
//! [`shader::Program`], then blit it with a trivial fullscreen pass — no id
//! churn, no atlas. This is the double-buffering the image widget can't give.
//!
//! The captured frame's aspect already matches the blade (`browser::fit_width`
//! plus the aspect-preserving capture cap), so filling the widget bounds with
//! it neither stretches nor letterboxes.

use std::sync::Arc;

use iced::mouse;
use iced::wgpu;
use iced::widget::shader::{self, Pipeline, Primitive};
use iced::{Rectangle, Size};

/// One decoded RGBA frame, cheap to clone (`Arc`) so `view` can hand it to the
/// program on every redraw without copying the pixels.
#[derive(Clone)]
pub struct FrameData {
    pub width: u32,
    pub height: u32,
    pub pixels: Arc<[u8]>,
}

/// The shader program: draws the newest frame. It carries no state of its own
/// beyond the frame — input and sizing live in the wrapping `screencast` widget.
pub struct BrowserProgram {
    pub frame: FrameData,
}

impl<Message> shader::Program<Message> for BrowserProgram {
    type State = ();
    type Primitive = FramePrimitive;

    fn draw(&self, _state: &(), _cursor: mouse::Cursor, _bounds: Rectangle) -> FramePrimitive {
        FramePrimitive {
            frame: self.frame.clone(),
        }
    }
}

/// The per-frame primitive: just the frame to upload. The heavy GPU state lives
/// in [`BrowserPipeline`], shared across every frame.
pub struct FramePrimitive {
    frame: FrameData,
}

impl std::fmt::Debug for FramePrimitive {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never dump the pixel buffer.
        f.debug_struct("FramePrimitive")
            .field("width", &self.frame.width)
            .field("height", &self.frame.height)
            .finish()
    }
}

impl Primitive for FramePrimitive {
    type Pipeline = BrowserPipeline;

    fn prepare(
        &self,
        pipeline: &mut BrowserPipeline,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _bounds: &Rectangle,
        _viewport: &shader::Viewport,
    ) {
        pipeline.upload(
            device,
            queue,
            Size::new(self.frame.width, self.frame.height),
            &self.frame.pixels,
        );
    }

    fn draw(&self, pipeline: &BrowserPipeline, render_pass: &mut wgpu::RenderPass<'_>) -> bool {
        // iced has already set the pass viewport + scissor to our widget bounds,
        // so a fullscreen triangle lands exactly in the blade.
        pipeline.draw(render_pass)
    }
}

/// One texture written in place each frame, and the pipeline that blits it.
pub struct BrowserPipeline {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    texture: Option<Texture>,
}

/// The current frame texture, recreated only when the frame's dimensions change
/// (a resize) — otherwise rewritten in place, which is the whole point.
struct Texture {
    size: Size<u32>,
    texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
}

const SHADER: &str = r#"
@group(0) @binding(0) var frame_texture: texture_2d<f32>;
@group(0) @binding(1) var frame_sampler: sampler;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> VertexOutput {
    // A single triangle covering the whole viewport; uv (0,0) at the top-left
    // matches the frame's row-0-is-top layout, so no vertical flip is needed.
    let x = f32((index << 1u) & 2u);
    let y = f32(index & 2u);
    var out: VertexOutput;
    out.position = vec4<f32>(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
    out.uv = vec2<f32>(x, y);
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    return textureSample(frame_texture, frame_sampler, in.uv);
}
"#;

impl Pipeline for BrowserPipeline {
    fn new(device: &wgpu::Device, _queue: &wgpu::Queue, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("junto browser blit shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("junto browser bind group layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("junto browser pipeline layout"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("junto browser pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    // The frame is opaque; replace, don't blend.
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("junto browser sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..wgpu::SamplerDescriptor::default()
        });

        Self {
            pipeline,
            bind_group_layout,
            sampler,
            texture: None,
        }
    }
}

impl BrowserPipeline {
    /// Ensure a texture of `size` exists (recreated only on a size change) and
    /// write this frame's pixels into it.
    fn upload(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        size: Size<u32>,
        pixels: &[u8],
    ) {
        if self.texture.as_ref().map(|t| t.size) != Some(size) {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("junto browser frame"),
                size: wgpu::Extent3d {
                    width: size.width,
                    height: size.height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                // sRGB texture: sampling decodes to linear and the sRGB surface
                // re-encodes on write, so the JPEG's sRGB bytes display as-is.
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("junto browser bind group"),
                layout: &self.bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                ],
            });
            self.texture = Some(Texture {
                size,
                texture,
                bind_group,
            });
        }

        // Guard against a truncated buffer (a decode hiccup) so `write_texture`
        // is never handed fewer bytes than the copy describes.
        let expected = size.width as usize * size.height as usize * 4;
        if let Some(slot) = &self.texture
            && pixels.len() >= expected
        {
            queue.write_texture(
                slot.texture.as_image_copy(),
                pixels,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(size.width * 4),
                    rows_per_image: Some(size.height),
                },
                wgpu::Extent3d {
                    width: size.width,
                    height: size.height,
                    depth_or_array_layers: 1,
                },
            );
        }
    }

    fn draw(&self, render_pass: &mut wgpu::RenderPass<'_>) -> bool {
        let Some(slot) = &self.texture else {
            return false;
        };
        render_pass.set_pipeline(&self.pipeline);
        render_pass.set_bind_group(0, &slot.bind_group, &[]);
        render_pass.draw(0..3, 0..1);
        true
    }
}
