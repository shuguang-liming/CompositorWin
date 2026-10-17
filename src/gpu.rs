use crate::model::{BlendMode, Document, Raster, Sampling, TILE_SIZE, Transform};
use eframe::egui::TextureId;
use egui_wgpu::{RenderState, wgpu};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    num::NonZeroU64,
    sync::Arc,
};
use uuid::Uuid;

struct MipEntry {
    buffer: wgpu::Buffer,
    binding: wgpu::BindGroup,
    capacity: usize,
}
struct Cached {
    texture: wgpu::Texture,
    pixels: Arc<Vec<u8>>,
    tiles: BTreeMap<(u32, u32), Arc<Vec<u8>>>,
    size: [u32; 2],
    generation: u64,
    mips: Vec<Option<MipEntry>>,
}
struct CompositeEntry {
    buffer: wgpu::Buffer,
    bindings: [wgpu::BindGroup; 2],
    source_generation: u64,
    mask_generation: Option<u64>,
}
pub struct GpuCanvas {
    state: RenderState,
    pipeline: wgpu::ComputePipeline,
    mipmap: wgpu::ComputePipeline,
    sources: HashMap<(Uuid, bool), Cached>,
    composites: HashMap<Uuid, CompositeEntry>,
    source_generation: u64,
    output: Option<[wgpu::Texture; 2]>,
    size: [u32; 2],
    id: Option<TextureId>,
    white: wgpu::Texture,
    last_output: usize,
    pub uploaded_bytes: u64,
    pub compositions: u64,
    pub binding_allocations: u64,
    pub mip_binding_allocations: u64,
    pub mip_dispatches: u64,
}
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    canvas: [f32; 4],
    layer: [f32; 4],
    rotate: [f32; 4],
    options: [f32; 4],
    mask: [f32; 4],
    mask_rotate: [f32; 4],
    mask_options: [f32; 4],
}
fn texture(
    device: &wgpu::Device,
    w: u32,
    h: u32,
    label: &str,
    single_level: bool,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: if single_level {
            1
        } else {
            32 - w.max(h).leading_zeros()
        },
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_DST
            | wgpu::TextureUsages::COPY_SRC
            | wgpu::TextureUsages::STORAGE_BINDING
            | wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    })
}
fn upload(queue: &wgpu::Queue, tex: &wgpu::Texture, origin: [u32; 2], w: u32, h: u32, data: &[u8]) {
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: tex,
            mip_level: 0,
            origin: wgpu::Origin3d {
                x: origin[0],
                y: origin[1],
                z: 0,
            },
            aspect: wgpu::TextureAspect::All,
        },
        data,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(w * 4),
            rows_per_image: Some(h),
        },
        wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
    );
}
fn premultiply(mut data: Vec<u8>) -> Vec<u8> {
    let convert = |pixels: &mut [u8]| {
        for p in pixels.as_chunks_mut::<4>().0 {
            let a = p[3] as u16;
            for c in &mut p[..3] {
                *c = ((*c as u16 * a + 127) / 255) as u8;
            }
        }
    };
    if data.len() >= 4_000_000 {
        use rayon::prelude::*;
        data.par_chunks_mut(256 * 1024).for_each(convert);
    } else {
        convert(&mut data);
    }
    data
}
fn scaled_region(r: &Raster, size: [u32; 2], rect: [u32; 4]) -> Vec<u8> {
    use rayon::prelude::*;
    let sx = r.width as f64 / size[0] as f64;
    let sy = r.height as f64 / size[1] as f64;
    let nx = (sx.ceil() as u32).clamp(1, 8);
    let ny = (sy.ceil() as u32).clamp(1, 8);
    let mut data = vec![0u8; (rect[2] * rect[3] * 4) as usize];
    let draw_row = |row: usize, pixels: &mut [u8]| {
        for (x, p) in pixels.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let mut total = [0u32; 4];
            for j in 0..ny {
                for i in 0..nx {
                    let px = ((rect[0] as f64 + x as f64 + (i as f64 + 0.5) / nx as f64) * sx)
                        .floor() as u32;
                    let py = ((rect[1] as f64 + row as f64 + (j as f64 + 0.5) / ny as f64) * sy)
                        .floor() as u32;
                    let q = r.get(px.min(r.width - 1), py.min(r.height - 1));
                    for c in 0..3 {
                        total[c] += (q[c] as u32 * q[3] as u32 + 127) / 255;
                    }
                    total[3] += q[3] as u32;
                }
            }
            for c in 0..4 {
                p[c] = ((total[c] + nx * ny / 2) / (nx * ny)) as u8;
            }
        }
    };
    // Small brush rectangles stay on the calling thread; worker startup/dispatch costs more than their sampling.
    if rect[2] as u64 * rect[3] as u64 * nx as u64 * ny as u64 >= 1_000_000 {
        data.par_chunks_mut((rect[2] * 4) as usize)
            .enumerate()
            .for_each(|(row, pixels)| draw_row(row, pixels));
    } else {
        for (row, pixels) in data.chunks_mut((rect[2] * 4) as usize).enumerate() {
            draw_row(row, pixels);
        }
    }
    data
}
fn transform(t: Transform) -> ([f32; 4], [f32; 4]) {
    let (s, c) = t.rotation.to_radians().sin_cos();
    (
        [
            t.origin[0] as f32,
            t.origin[1] as f32,
            t.size[0] as f32,
            t.size[1] as f32,
        ],
        [
            c as f32,
            s as f32,
            if t.flip_x { -1. } else { 1. },
            if t.flip_y { -1. } else { 1. },
        ],
    )
}
fn source_budget(count: usize) -> u32 {
    // At most 512 MiB of layer RGBA textures including the 4/3 mipmap overhead.
    (512. * 1024. * 1024. / (count.max(1) as f64 * 4. * 4. / 3.))
        .sqrt()
        .floor() as u32
}
impl GpuCanvas {
    pub fn new(state: RenderState) -> Self {
        let shader = state
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("Compositor blend kernel"),
                source: wgpu::ShaderSource::Wgsl(include_str!("../assets/composite.wgsl").into()),
            });
        let pipeline = state
            .device
            .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("Compositor compositing"),
                layout: None,
                module: &shader,
                entry_point: Some("composite"),
                compilation_options: Default::default(),
                cache: None,
            });
        let mipmap_shader = state
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("Premultiplied mipmap reduction"),
                source: wgpu::ShaderSource::Wgsl(include_str!("../assets/mipmap.wgsl").into()),
            });
        let mipmap_bindings =
            state
                .device
                .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("Resident mipmap bindings"),
                    entries: &[
                        wgpu::BindGroupLayoutEntry {
                            binding: 0,
                            visibility: wgpu::ShaderStages::COMPUTE,
                            ty: wgpu::BindingType::Texture {
                                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                                view_dimension: wgpu::TextureViewDimension::D2,
                                multisampled: false,
                            },
                            count: None,
                        },
                        wgpu::BindGroupLayoutEntry {
                            binding: 1,
                            visibility: wgpu::ShaderStages::COMPUTE,
                            ty: wgpu::BindingType::StorageTexture {
                                access: wgpu::StorageTextureAccess::WriteOnly,
                                format: wgpu::TextureFormat::Rgba8Unorm,
                                view_dimension: wgpu::TextureViewDimension::D2,
                            },
                            count: None,
                        },
                        wgpu::BindGroupLayoutEntry {
                            binding: 2,
                            visibility: wgpu::ShaderStages::COMPUTE,
                            ty: wgpu::BindingType::Buffer {
                                ty: wgpu::BufferBindingType::Uniform,
                                has_dynamic_offset: true,
                                min_binding_size: NonZeroU64::new(16),
                            },
                            count: None,
                        },
                    ],
                });
        let mipmap_layout = state
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("Resident mipmap layout"),
                bind_group_layouts: &[&mipmap_bindings],
                push_constant_ranges: &[],
            });
        let mipmap = state
            .device
            .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("Incremental mipmap"),
                layout: Some(&mipmap_layout),
                module: &mipmap_shader,
                entry_point: Some("reduce"),
                compilation_options: Default::default(),
                cache: None,
            });
        let white = texture(&state.device, 1, 1, "No mask", false);
        upload(&state.queue, &white, [0, 0], 1, 1, &[255; 4]);
        Self {
            state,
            pipeline,
            mipmap,
            sources: HashMap::new(),
            composites: HashMap::new(),
            source_generation: 0,
            output: None,
            size: [0, 0],
            id: None,
            white,
            last_output: 0,
            uploaded_bytes: 0,
            compositions: 0,
            binding_allocations: 0,
            mip_binding_allocations: 0,
            mip_dispatches: 0,
        }
    }
    pub fn supports(d: &Document) -> bool {
        d.layers.iter().all(|l| {
            l.adjustment.is_none()
                && l.image.as_ref().is_none_or(|r| r.deep.is_none())
                && l.effects.is_none()
                && l.mask_source_id.is_none()
                && (!l.is_group || l.mask.is_none())
        })
    }
    fn update_source(&mut self, key: (Uuid, bool), r: &Raster, budget_limit: u32) {
        let limit = self
            .state
            .device
            .limits()
            .max_texture_dimension_2d
            .min(8192)
            .min(budget_limit);
        let scaled = r.width > limit || r.height > limit;
        let factor = (limit as f64 / r.width.max(r.height) as f64).min(1.);
        let size = [
            (r.width as f64 * factor).round().max(1.) as u32,
            (r.height as f64 * factor).round().max(1.) as u32,
        ];
        let replace = self.sources.get(&key).is_none_or(|c| c.size != size);
        if replace {
            self.composites.remove(&key.0);
            self.source_generation = self.source_generation.wrapping_add(1);
            self.sources.insert(
                key,
                Cached {
                    // Mask sampling reads only level zero; its unused mip chain would waste VRAM and brush work.
                    texture: texture(&self.state.device, size[0], size[1], "Layer source", key.1),
                    pixels: Arc::new(Vec::new()),
                    tiles: BTreeMap::new(),
                    size,
                    generation: self.source_generation,
                    mips: Vec::new(),
                },
            );
        }
        let c = self.sources.get_mut(&key).unwrap();
        let mut dirty = Vec::new();
        let base_changed = replace || !Arc::ptr_eq(&c.pixels, &r.pixels);
        let tiles_changed = c.tiles.len() != r.tiles.len()
            || r.tiles
                .iter()
                .any(|(k, v)| c.tiles.get(k).is_none_or(|old| !Arc::ptr_eq(old, v)));
        if scaled && r.pixels.len() == 4 && r.tiles.is_empty() {
            if base_changed || tiles_changed {
                clear_uniform(
                    &self.state.device,
                    &self.state.queue,
                    &c.texture,
                    r.get(0, 0),
                );
                self.uploaded_bytes += 4;
                dirty.push([0, 0, size[0], size[1]]);
            }
        } else if scaled && (base_changed || tiles_changed) {
            let regions = if base_changed {
                vec![[0, 0, size[0], size[1]]]
            } else {
                let keys: std::collections::BTreeSet<_> =
                    c.tiles.keys().chain(r.tiles.keys()).copied().collect();
                keys.into_iter()
                    .filter(|key| match (c.tiles.get(key), r.tiles.get(key)) {
                        (Some(a), Some(b)) => !Arc::ptr_eq(a, b),
                        _ => true,
                    })
                    .filter_map(|key| {
                        let x = (key.0 * TILE_SIZE) as f64 * size[0] as f64 / r.width as f64;
                        let y = (key.1 * TILE_SIZE) as f64 * size[1] as f64 / r.height as f64;
                        let right =
                            ((key.0 + 1) * TILE_SIZE) as f64 * size[0] as f64 / r.width as f64;
                        let bottom =
                            ((key.1 + 1) * TILE_SIZE) as f64 * size[1] as f64 / r.height as f64;
                        let x0 = x.floor() as u32;
                        let y0 = y.floor() as u32;
                        let x1 = (right.ceil() as u32).min(size[0]);
                        let y1 = (bottom.ceil() as u32).min(size[1]);
                        (x0 < x1 && y0 < y1).then_some([x0, y0, x1 - x0, y1 - y0])
                    })
                    .collect()
            };
            for rect in regions {
                let data = scaled_region(r, size, rect);
                upload(
                    &self.state.queue,
                    &c.texture,
                    [rect[0], rect[1]],
                    rect[2],
                    rect[3],
                    &data,
                );
                self.uploaded_bytes += data.len() as u64;
                dirty.push(rect);
            }
        } else if !scaled {
            let removed = c.tiles.keys().any(|k| !r.tiles.contains_key(k));
            let mut full_materialized_upload = false;
            let mut reset_texture = false;
            if base_changed || removed {
                if r.pixels.len() == 4 || r.pixels.is_empty() {
                    clear_uniform(
                        &self.state.device,
                        &self.state.queue,
                        &c.texture,
                        if r.pixels.len() == 4 {
                            r.pixels.as_slice().try_into().unwrap()
                        } else {
                            [0; 4]
                        },
                    );
                    self.uploaded_bytes += 4;
                    dirty.push([0, 0, size[0], size[1]]);
                    reset_texture = true;
                } else if !r.pixels.is_empty() || removed {
                    let data = premultiply(r.to_rgba());
                    upload(
                        &self.state.queue,
                        &c.texture,
                        [0, 0],
                        size[0],
                        size[1],
                        &data,
                    );
                    self.uploaded_bytes += data.len() as u64;
                    dirty.push([0, 0, size[0], size[1]]);
                    full_materialized_upload = true;
                }
            }
            // A materialized upload already includes all tiles. A uniform clear removes
            // all cached tile contents, even when their Arc identities did not change.
            if !full_materialized_upload {
                for (k, v) in &r.tiles {
                    if !reset_texture && c.tiles.get(k).is_some_and(|old| Arc::ptr_eq(old, v)) {
                        continue;
                    }
                    let x = k.0 * TILE_SIZE;
                    let y = k.1 * TILE_SIZE;
                    let w = TILE_SIZE.min(r.width - x);
                    let h = TILE_SIZE.min(r.height - y);
                    let mut data = Vec::with_capacity((w * h * 4) as usize);
                    for row in 0..h {
                        let start = (row * TILE_SIZE * 4) as usize;
                        data.extend_from_slice(&v[start..start + (w * 4) as usize]);
                    }
                    let data = premultiply(data);
                    upload(&self.state.queue, &c.texture, [x, y], w, h, &data);
                    self.uploaded_bytes += data.len() as u64;
                    if !reset_texture {
                        dirty.push([x, y, w, h]);
                    }
                }
            }
        }
        if base_changed {
            c.pixels = r.pixels.clone();
        }
        if tiles_changed {
            c.tiles = r.tiles.clone();
        }
        if !dirty.is_empty() {
            regenerate_mips(
                &self.state.device,
                &self.state.queue,
                &self.mipmap,
                c,
                &dirty,
                &mut self.mip_binding_allocations,
                &mut self.mip_dispatches,
            );
        }
    }
    pub fn render(&mut self, d: &Document, max_side: u32) -> TextureId {
        let factor = (max_side as f64 / d.width.max(d.height) as f64).min(1.);
        let size = [
            (d.width as f64 * factor).round().max(1.) as u32,
            (d.height as f64 * factor).round().max(1.) as u32,
        ];
        self.render_sized(d, size, wgpu::FilterMode::Linear)
    }
    /// Render an original-pixel viewport. Oversized sources use the CPU path rather
    /// than silently replacing original detail with a budget-sized source texture.
    pub fn render_view(
        &mut self,
        doc: &Document,
        view: crate::preview::ViewRequest,
    ) -> Option<TextureId> {
        if !Self::supports(doc) {
            self.composites.clear();
            self.sources.clear();
            return None;
        }
        let mut order = Vec::new();
        ordered_layers(doc, None, 0, &mut order);
        let sources: Vec<_> = order
            .iter()
            .flat_map(|layer| {
                layer.image.iter().chain(
                    layer
                        .mask
                        .iter()
                        .filter(|mask| mask.enabled)
                        .map(|mask| &mask.raster),
                )
            })
            .collect();
        let device_limit = self.state.device.limits().max_texture_dimension_2d;
        let source_limit = device_limit.min(8192).min(source_budget(sources.len()));
        if view.width > device_limit
            || view.height > device_limit
            || sources
                .iter()
                .any(|raster| raster.width.max(raster.height) > source_limit)
        {
            self.composites.clear();
            self.sources.clear();
            return None;
        }
        let mut viewport = doc.clone();
        viewport.width = view.region[2];
        viewport.height = view.region[3];
        viewport.selection = None;
        for layer in &mut viewport.layers {
            layer.transform.origin[0] -= view.region[0] as f64;
            layer.transform.origin[1] -= view.region[1] as f64;
            if let Some(placement) = layer.mask.as_mut().and_then(|mask| mask.placement.as_mut()) {
                placement.origin[0] -= view.region[0] as f64;
                placement.origin[1] -= view.region[1] as f64;
            }
        }
        Some(self.render_sized(
            &viewport,
            [view.width, view.height],
            if view.native() {
                wgpu::FilterMode::Nearest
            } else {
                wgpu::FilterMode::Linear
            },
        ))
    }
    fn render_sized(
        &mut self,
        d: &Document,
        size: [u32; 2],
        filter: wgpu::FilterMode,
    ) -> TextureId {
        if self.size != size {
            self.composites.clear();
            self.output = Some([
                texture(&self.state.device, size[0], size[1], "Composite A", true),
                texture(&self.state.device, size[0], size[1], "Composite B", true),
            ]);
            self.size = size;
        }
        let mut used = HashSet::new();
        let mut order = Vec::new();
        ordered_layers(d, None, 0, &mut order);
        for l in &order {
            if l.image.is_some() {
                used.insert((l.id, false));
            }
            if l.mask.as_ref().is_some_and(|m| m.enabled) {
                used.insert((l.id, true));
            }
        }
        // Release textures from the previous document before allocating the next document's sources.
        self.composites.retain(|id, _| used.contains(&(*id, false)));
        self.sources.retain(|k, _| used.contains(k));
        let count = order
            .iter()
            .map(|l| {
                usize::from(l.image.is_some())
                    + usize::from(l.mask.as_ref().is_some_and(|m| m.enabled))
            })
            .sum::<usize>()
            .max(1);
        let budget_limit = source_budget(count);
        for l in &order {
            if let Some(r) = &l.image {
                self.update_source((l.id, false), r, budget_limit);
                used.insert((l.id, false));
            }
            if let Some(m) = &l.mask {
                if !m.enabled {
                    continue;
                }
                self.update_source((l.id, true), &m.raster, budget_limit);
                used.insert((l.id, true));
            }
        }
        let output = self.output.as_ref().unwrap();
        let mut encoder =
            self.state
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("Composite document"),
                });
        let view = output[0].create_view(&Default::default());
        {
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Clear canvas"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
        }
        let mut current = 0;
        for l in order {
            if !l.visible || l.is_group || l.image.is_none() {
                continue;
            }
            let mut opacity = l.opacity;
            let mut parent = l.parent_id;
            let mut visible = true;
            for _ in 0..64 {
                let Some(id) = parent else {
                    break;
                };
                let Some(p) = d.layer(id) else {
                    break;
                };
                opacity *= p.opacity;
                visible &= p.visible;
                parent = p.parent_id;
            }
            if !visible || opacity <= 0. {
                continue;
            }
            let (layer, rotate) = transform(l.transform);
            let (mask, mask_rotate) = transform(
                l.mask
                    .as_ref()
                    .and_then(|m| m.placement)
                    .unwrap_or(l.transform),
            );
            let mode = BlendMode::ALL
                .iter()
                .position(|m| *m == l.blend_mode)
                .unwrap_or(0);
            let p = Params {
                canvas: [
                    d.width as f32,
                    d.height as f32,
                    size[0] as f32,
                    size[1] as f32,
                ],
                layer,
                rotate,
                mask,
                mask_rotate,
                mask_options: [
                    l.mask.as_ref().map_or(1.0, |m| {
                        crate::render::cached_mask_background(&m.raster) as f32 / 255.
                    }),
                    l.mask.as_ref().map_or(1., |m| {
                        match m.placement.as_ref().unwrap_or(&l.transform).sampling {
                            Sampling::Nearest => 0.,
                            Sampling::Smooth => 1.,
                            Sampling::High => 2.,
                        }
                    }),
                    0.,
                    0.,
                ],
                options: [
                    opacity as f32,
                    mode as f32,
                    if l.mask.as_ref().is_some_and(|m| m.enabled) {
                        1.
                    } else {
                        0.
                    },
                    if l.transform.sampling == Sampling::Nearest {
                        0.
                    } else if l.transform.sampling == Sampling::Smooth {
                        1.
                    } else {
                        2.
                    },
                ],
            };
            let source = &self.sources[&(l.id, false)];
            let mask_source = self.sources.get(&(l.id, true));
            let mask_generation = mask_source.map(|c| c.generation);
            let reusable = self.composites.get(&l.id).is_some_and(|entry| {
                entry.source_generation == source.generation
                    && entry.mask_generation == mask_generation
            });
            if !reusable {
                let buffer = self.state.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("Layer transform"),
                    size: std::mem::size_of::<Params>() as u64,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                let source_view = source.texture.create_view(&Default::default());
                let coverage = mask_source
                    .map(|c| &c.texture)
                    .unwrap_or(&self.white)
                    .create_view(&Default::default());
                let views = output
                    .each_ref()
                    .map(|t| t.create_view(&Default::default()));
                let layout = self.pipeline.get_bind_group_layout(0);
                let bindings = std::array::from_fn(|index| {
                    self.state
                        .device
                        .create_bind_group(&wgpu::BindGroupDescriptor {
                            label: Some("Layer composite"),
                            layout: &layout,
                            entries: &[
                                wgpu::BindGroupEntry {
                                    binding: 0,
                                    resource: wgpu::BindingResource::TextureView(&source_view),
                                },
                                wgpu::BindGroupEntry {
                                    binding: 1,
                                    resource: wgpu::BindingResource::TextureView(&views[index]),
                                },
                                wgpu::BindGroupEntry {
                                    binding: 2,
                                    resource: wgpu::BindingResource::TextureView(&coverage),
                                },
                                wgpu::BindGroupEntry {
                                    binding: 3,
                                    resource: wgpu::BindingResource::TextureView(&views[1 - index]),
                                },
                                wgpu::BindGroupEntry {
                                    binding: 5,
                                    resource: buffer.as_entire_binding(),
                                },
                            ],
                        })
                });
                self.composites.insert(
                    l.id,
                    CompositeEntry {
                        buffer,
                        bindings,
                        source_generation: source.generation,
                        mask_generation,
                    },
                );
                self.binding_allocations += 1;
            }
            let entry = &self.composites[&l.id];
            self.state
                .queue
                .write_buffer(&entry.buffer, 0, bytemuck::bytes_of(&p));
            {
                let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("Blend layer"),
                    timestamp_writes: None,
                });
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &entry.bindings[current], &[]);
                pass.dispatch_workgroups(size[0].div_ceil(8), size[1].div_ceil(8), 1);
            }
            current = 1 - current;
        }
        self.state.queue.submit([encoder.finish()]);
        self.last_output = current;
        self.compositions += 1;
        let view = output[current].create_view(&Default::default());
        let mut renderer = self.state.renderer.write();
        if let Some(id) = self.id {
            renderer.update_egui_texture_from_wgpu_texture(&self.state.device, &view, filter, id);
            id
        } else {
            let id = renderer.register_native_texture(&self.state.device, &view, filter);
            self.id = Some(id);
            id
        }
    }
    pub fn adapter_info(&self) -> String {
        let info = self.state.adapter.get_info();
        format!("{} · {:?}", info.name, info.backend)
    }
    /// Allocated layer/mask texture bytes, including their resident mip levels.
    pub fn resident_source_bytes(&self) -> u64 {
        self.sources
            .values()
            .map(|c| {
                (0..c.texture.mip_level_count())
                    .map(|level| {
                        (c.size[0] >> level).max(1) as u64 * (c.size[1] >> level).max(1) as u64 * 4
                    })
                    .sum::<u64>()
            })
            .sum()
    }
    /// Read the rendered premultiplied surface for export/parity tests. Interactive painting stays GPU-resident.
    pub fn readback(&self) -> Result<Raster, String> {
        let output = self
            .output
            .as_ref()
            .ok_or("Render a canvas before reading it")?;
        let [width, height] = self.size;
        let stride = (width * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
            * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let buffer = self.state.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Composite readback"),
            size: stride as u64 * height as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self
            .state
            .device
            .create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &output[self.last_output],
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(stride),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        let submission = self.state.queue.submit([encoder.finish()]);
        let (tx, rx) = std::sync::mpsc::channel();
        buffer.slice(..).map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        self.state
            .device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(submission),
                timeout: Some(std::time::Duration::from_secs(30)),
            })
            .map_err(|e| format!("GPU readback: {e}"))?;
        rx.recv_timeout(std::time::Duration::from_secs(30))
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())?;
        let mapped = buffer.slice(..).get_mapped_range();
        let mut pixels = Vec::with_capacity((width * height * 4) as usize);
        for row in mapped.chunks_exact(stride as usize) {
            pixels.extend_from_slice(&row[..width as usize * 4]);
        }
        drop(mapped);
        buffer.unmap();
        crate::pixels::unpremultiply(&mut pixels);
        Ok(Raster::from_rgba(width, height, pixels))
    }
}
fn clear_uniform(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    color: [u8; 4],
) {
    let alpha = color[3] as f64 / 255.0;
    let c = wgpu::Color {
        r: color[0] as f64 / 255.0 * alpha,
        g: color[1] as f64 / 255.0 * alpha,
        b: color[2] as f64 / 255.0 * alpha,
        a: alpha,
    };
    let view = texture.create_view(&wgpu::TextureViewDescriptor {
        base_mip_level: 0,
        mip_level_count: Some(1),
        ..Default::default()
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("Uniform raster GPU fill"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(c),
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
    }
    queue.submit([encoder.finish()]);
}
fn regenerate_mips(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipeline: &wgpu::ComputePipeline,
    source: &mut Cached,
    rects: &[[u32; 4]],
    allocations: &mut u64,
    dispatches: &mut u64,
) {
    let texture = &source.texture;
    if texture.mip_level_count() <= 1 {
        return;
    }
    source
        .mips
        .resize_with(texture.mip_level_count() as usize - 1, || None);
    let stride = 16_u32.div_ceil(device.limits().min_uniform_buffer_offset_alignment)
        * device.limits().min_uniform_buffer_offset_alignment;
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("Regenerate dirty mipmaps"),
    });
    let mut regions = rects.to_vec();
    for level in 1..texture.mip_level_count() {
        let previous_size = [
            (texture.width() >> (level - 1)).max(1),
            (texture.height() >> (level - 1)).max(1),
        ];
        let next_size = [
            (texture.width() >> level).max(1),
            (texture.height() >> level).max(1),
        ];
        let ratio = [
            next_size[0] as f64 / previous_size[0] as f64,
            next_size[1] as f64 / previous_size[1] as f64,
        ];
        for rect in &mut regions {
            let x = (rect[0] as f64 * ratio[0]).floor() as u32;
            let y = (rect[1] as f64 * ratio[1]).floor() as u32;
            let right = (((rect[0] + rect[2]) as f64 * ratio[0]).ceil() as u32).min(next_size[0]);
            let bottom = (((rect[1] + rect[3]) as f64 * ratio[1]).ceil() as u32).min(next_size[1]);
            *rect = [x, y, right - x, bottom - y];
        }
        // Merge only exact rectangular unions; separated edits never grow into a full-image update.
        coalesce_regions(&mut regions);
        let slot = &mut source.mips[level as usize - 1];
        if slot
            .as_ref()
            .is_none_or(|entry| entry.capacity < regions.len())
        {
            let capacity = regions.len().next_power_of_two();
            let src = texture.create_view(&wgpu::TextureViewDescriptor {
                base_mip_level: level - 1,
                mip_level_count: Some(1),
                ..Default::default()
            });
            let dest = texture.create_view(&wgpu::TextureViewDescriptor {
                base_mip_level: level,
                mip_level_count: Some(1),
                ..Default::default()
            });
            let uniform = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("Resident dirty mip rectangles"),
                size: capacity as u64 * stride as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("Mipmap pass"),
                layout: &pipeline.get_bind_group_layout(0),
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&src),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&dest),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                            buffer: &uniform,
                            offset: 0,
                            size: NonZeroU64::new(16),
                        }),
                    },
                ],
            });
            *slot = Some(MipEntry {
                buffer: uniform,
                binding: bind,
                capacity,
            });
            *allocations += 1;
        }
        let entry = slot.as_ref().unwrap();
        let mut parameters = vec![0_u8; (regions.len() - 1) * stride as usize + 16];
        for (index, rect) in regions.iter().enumerate() {
            let offset = index * stride as usize;
            parameters[offset..offset + 16].copy_from_slice(bytemuck::cast_slice(rect));
        }
        queue.write_buffer(&entry.buffer, 0, &parameters);
        for (index, rect) in regions.iter().enumerate() {
            let mut pass = encoder.begin_compute_pass(&Default::default());
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &entry.binding, &[index as u32 * stride]);
            pass.dispatch_workgroups(rect[2].div_ceil(8), rect[3].div_ceil(8), 1);
            *dispatches += 1;
        }
    }
    queue.submit([encoder.finish()]);
}
fn coalesce_regions(rects: &mut Vec<[u32; 4]>) {
    for axis in [0, 1] {
        let other = 1 - axis;
        rects.sort_unstable_by_key(|r| (r[other], r[other + 2], r[axis]));
        let mut count = 0;
        for index in 0..rects.len() {
            let rect = rects[index];
            if count > 0 {
                let previous = &mut rects[count - 1];
                if previous[other] == rect[other]
                    && previous[other + 2] == rect[other + 2]
                    && rect[axis] <= previous[axis] + previous[axis + 2]
                {
                    previous[axis + 2] = (previous[axis] + previous[axis + 2])
                        .max(rect[axis] + rect[axis + 2])
                        - previous[axis];
                    continue;
                }
            }
            rects[count] = rect;
            count += 1;
        }
        rects.truncate(count);
    }
}
fn ordered_layers<'a>(
    d: &'a Document,
    parent: Option<Uuid>,
    depth: usize,
    out: &mut Vec<&'a crate::model::Layer>,
) {
    if depth > 64 {
        return;
    }
    for l in d
        .layers
        .iter()
        .filter(|l| l.parent_id == parent && l.visible && l.opacity > 0.0)
    {
        if l.is_group {
            ordered_layers(d, Some(l.id), depth + 1, out);
        } else {
            out.push(l);
        }
    }
}
impl Drop for GpuCanvas {
    fn drop(&mut self) {
        if let Some(id) = self.id {
            self.state.renderer.write().free_texture(&id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::coalesce_regions;

    #[test]
    fn dirty_region_merging_preserves_exact_coverage() {
        for original in [
            vec![[0, 0, 4, 4], [4, 0, 4, 4], [0, 4, 4, 4], [4, 4, 4, 4]],
            vec![[0, 0, 4, 4], [3, 0, 4, 4], [0, 3, 7, 4]],
            vec![[0, 0, 4, 4], [4, 4, 4, 4]],
            vec![[0, 0, 4, 8], [4, 0, 4, 4]],
            vec![[1, 1, 1, 1], [13, 13, 2, 2], [13, 13, 2, 2]],
        ] {
            let mut merged = original.clone();
            coalesce_regions(&mut merged);
            let covered = |rects: &[[u32; 4]], x, y| {
                rects
                    .iter()
                    .any(|r| x >= r[0] && x < r[0] + r[2] && y >= r[1] && y < r[1] + r[3])
            };
            for y in 0..16 {
                for x in 0..16 {
                    assert_eq!(
                        covered(&original, x, y),
                        covered(&merged, x, y),
                        "Merging {original:?} into {merged:?} changed pixel {x},{y}"
                    );
                }
            }
            assert!(merged.len() < original.len() || original.len() == 2);
        }
    }
}
