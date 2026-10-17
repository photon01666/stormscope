//! 3D volume rendering — the GPU side of the 3D radar pane.
//!
//! The pane shows all cuts of one volume scan as a rotatable scene: the mesh
//! comes from [`radar_render::volume3d`] (pure geometry, unit-tested there),
//! this module owns the wgpu resources and draws it into an offscreen
//! `Rgba8Unorm` texture that egui displays like any other image.
//!
//! Three passes, in this order:
//!
//! 1. **Ground** — the same basemap tiles the 2D map uses, drawn as textured
//!    quads at z = 0. Opaque, depth-written: it anchors the volume to real
//!    geography (cities, highways, county lines).
//! 2. **Grid** — the mile range rings + spokes from
//!    [`radar_render::volume3d::build_ground_grid`], translucent, no depth
//!    writes.
//! 3. **Volume** — the cuts, ascending elevation (back-to-front seen from
//!    above), translucent, no depth writes so overlapping layers blend like
//!    glass instead of punching holes into each other.
//!
//! Why offscreen instead of an egui paint callback: the scene needs a depth
//! buffer and its own clear/blend policy, and eframe's UI pass has no depth
//! attachment. An offscreen target gives full control with no eframe
//! configuration games, and registering it as an egui texture costs one
//! bind group.
//!
//! Compositing contract: egui treats user textures as *gamma-space*
//! (`Rgba8Unorm`, values composited as-is — `egui-wgpu`'s own docs say so
//! and its shader has a dedicated gamma-framebuffer path). So the whole
//! pipeline shades in display space, exactly like the 2D rasters and the map
//! tiles: no linearization anywhere, and what you see in 3D matches both.
//! Vertex colors arrive premultiplied; the fragment shader multiplies the
//! user's opacity in and outputs premultiplied RGBA, which is what egui's
//! blend state expects.
//!
//! The altitude slab (`alt_min..alt_max`, km) and the echo threshold are
//! fragment-shader discards, so both sliders are free — no mesh rebuilds.

use std::collections::HashMap;
use std::ops::Range;

use radar_core::{Moment, Volume};
use radar_render::volume3d::{
    Camera, MeshFilter, VolumeMesh, build_ground_grid, build_volume_mesh,
};

/// Offscreen target format. Must stay `Rgba8Unorm` — the only format
/// egui-wgpu accepts for user-registered textures.
const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
/// Vertex stride of `MeshVertex`: 3+3 floats + 4 bytes color + 1 float.
const VOLUME_VERTEX_STRIDE: u64 = 32;
/// Vertex stride of `GroundVertex`: 3 floats position + 2 floats uv.
const GROUND_VERTEX_STRIDE: u64 = 20;
/// Ground quad capacity: 2048 tiles is far beyond any real view.
const GROUND_VERT_CAP: usize = 8192;
const GROUND_INDEX_CAP: usize = 12288;
/// Per-tile bind groups are cheap but hold GPU textures alive; past this
/// many, keep only what the current frame draws.
const TILE_BG_CAP: usize = 400;

/// `view_proj` (64 bytes) + `params` + `params2` (16 bytes each).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    view_proj: [[f32; 4]; 4],
    /// `[vertical exaggeration, opacity, value threshold, unused]`.
    params: [f32; 4],
    /// `[min altitude km, max altitude km, grid line half-width (render px),
    /// unused]`. The grid/ground get `0..1e9` so they can never be discarded
    /// by the slab.
    params2: [f32; 4],
    /// `[eye.x, eye.y, eye.z, viewport height px]` — the ground grid's
    /// screen-space minimum line width is derived from these.
    params3: [f32; 4],
}

/// One ground-quad vertex (km, scene-local ENU; uv into the tile texture).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GroundVertex {
    pos_km: [f32; 3],
    uv: [f32; 2],
}

pub struct Volume3d {
    ground_bgl: wgpu::BindGroupLayout,
    volume_pipeline: wgpu::RenderPipeline,
    ground_pipeline: wgpu::RenderPipeline,
    volume_ub: wgpu::Buffer,
    grid_ub: wgpu::Buffer,
    volume_bg: wgpu::BindGroup,
    grid_bg: wgpu::BindGroup,
    /// 1×1 white: satisfies the ground texture slot for the volume/grid
    /// passes so the pipeline layouts stay identical.
    dummy_bg: wgpu::BindGroup,
    ground_sampler: wgpu::Sampler,

    /// Scene identity the current buffers were built from.
    scene_sig: Option<u64>,
    /// Mesh builds happen on a worker thread: playback asks for the frame it
    /// needs, keeps drawing the last one, and swaps the moment the build
    /// lands. The UI thread never waits on a mesh (a Fine rebuild is ~50 ms
    /// — far too much for an 8 fps tick, and it never has to be paid on the
    /// UI thread at all).
    job_tx: std::sync::mpsc::Sender<MeshJob>,
    done_rx: std::sync::mpsc::Receiver<MeshDone>,
    wanted: Option<MeshJob>,
    in_flight: Option<u64>,
    vbuf: wgpu::Buffer,
    ibuf: wgpu::Buffer,
    /// Allocated capacities: playback rebuilds the scene every tick, and
    /// same-shaped scans produce identical sizes — so the buffers are
    /// written into instead of re-allocated once they fit.
    vbuf_cap: u64,
    ibuf_cap: u64,
    /// Index ranges of the cuts, ascending elevation.
    tilt_ranges: Vec<Range<u32>>,
    /// Index range of the ground grid (drawn before the cuts).
    grid_range: Range<u32>,

    /// Basemap tiles: cached bind groups by egui texture id, plus the
    /// frame's quad geometry (rebuilt every frame — a few hundred vertices).
    tile_bgs: HashMap<egui::TextureId, wgpu::BindGroup>,
    ground_vbuf: wgpu::Buffer,
    ground_ibuf: wgpu::Buffer,
    ground_verts: Vec<GroundVertex>,
    ground_indices: Vec<u32>,
    ground_draws: Vec<(egui::TextureId, Range<u32>)>,

    max_range_km: f32,
    tilt_count: usize,
    triangles: usize,
    vertex_count: usize,
    elev_lo_deg: f32,
    elev_hi_deg: f32,
    moment_seen: Option<Moment>,

    color: Option<wgpu::Texture>,
    color_view: Option<wgpu::TextureView>,
    depth_view: Option<wgpu::TextureView>,
    size: [u32; 2],
    tex_id: Option<egui::TextureId>,
    tex_dirty: bool,
    /// When the current scene buffers were built — the rate limit for
    /// scrub-driven rebuilds (each one is a ~1M-vertex mesh plus ~50 MB of
    /// buffer traffic; right for "a volume arrived", wrong for a scrubber).
    scene_built_at: std::time::Instant,
    /// Change key of the last rendered frame, the last *full-resolution*
    /// render, and when the key last changed — the "render on demand, half
    /// res while moving" policy (the pane is GPU-bound at 4K: re-rendering
    /// the same camera every UI repaint burns the frame budget for nothing).
    last_key: Option<u64>,
    last_full_key: Option<u64>,
    last_change: std::time::Instant,
    /// Frames actually rendered (telemetry for the app's stats line).
    pub render_count: u64,
}

impl Volume3d {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        let cam_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("stormscope.volume3d.camera"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let ground_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("stormscope.volume3d.ground"),
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

        let make_ub = |label: &str| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: std::mem::size_of::<Uniforms>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        };
        let volume_ub = make_ub("stormscope.volume3d.volume-uniforms");
        let grid_ub = make_ub("stormscope.volume3d.grid-uniforms");
        let make_ub_bg = |label: &str, ub: &wgpu::Buffer| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(label),
                layout: &cam_bgl,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: ub.as_entire_binding(),
                }],
            })
        };
        let volume_bg = make_ub_bg("stormscope.volume3d.volume-bg", &volume_ub);
        let grid_bg = make_ub_bg("stormscope.volume3d.grid-bg", &grid_ub);

        // 1×1 white for the ground slot during grid/volume passes.
        let white = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("stormscope.volume3d.dummy"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: COLOR_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &white,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &[255, 255, 255, 255],
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4),
                rows_per_image: Some(1),
            },
            wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        let ground_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("stormscope.volume3d.ground-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let dummy_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("stormscope.volume3d.dummy-bg"),
            layout: &ground_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(
                        &white.create_view(&wgpu::TextureViewDescriptor::default()),
                    ),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&ground_sampler),
                },
            ],
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("stormscope.volume3d.shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });

        let volume_pipeline = {
            let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("stormscope.volume3d.volume-layout"),
                bind_group_layouts: &[Some(&cam_bgl), Some(&ground_bgl)],
                immediate_size: 0,
            });
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("stormscope.volume3d.volume-pipeline"),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    buffers: &[Some(wgpu::VertexBufferLayout {
                        array_stride: VOLUME_VERTEX_STRIDE,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &wgpu::vertex_attr_array![
                            0 => Float32x3, // position (km)
                            1 => Float32x3, // normal
                            2 => Unorm8x4,  // premultiplied color
                            3 => Float32,   // physical value
                        ],
                    })],
                },
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    strip_index_format: None,
                    front_face: wgpu::FrontFace::Ccw,
                    cull_mode: None, // translucent surfaces are seen from both sides
                    unclipped_depth: false,
                    polygon_mode: wgpu::PolygonMode::Fill,
                    conservative: false,
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    // Blending, not occlusion: overlapping cuts read as glass.
                    depth_write_enabled: Some(false),
                    depth_compare: Some(wgpu::CompareFunction::LessEqual),
                    stencil: wgpu::StencilState::default(),
                    bias: wgpu::DepthBiasState::default(),
                }),
                multisample: wgpu::MultisampleState {
                    count: 1,
                    mask: !0,
                    alpha_to_coverage_enabled: false,
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs_main"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: COLOR_FORMAT,
                        // Premultiplied alpha — egui's convention.
                        blend: Some(wgpu::BlendState {
                            color: wgpu::BlendComponent {
                                src_factor: wgpu::BlendFactor::One,
                                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                                operation: wgpu::BlendOperation::Add,
                            },
                            alpha: wgpu::BlendComponent {
                                src_factor: wgpu::BlendFactor::OneMinusDstAlpha,
                                dst_factor: wgpu::BlendFactor::One,
                                operation: wgpu::BlendOperation::Add,
                            },
                        }),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };

        let ground_pipeline = {
            let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("stormscope.volume3d.ground-layout"),
                bind_group_layouts: &[Some(&cam_bgl), Some(&ground_bgl)],
                immediate_size: 0,
            });
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("stormscope.volume3d.ground-pipeline"),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("gvs_main"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    buffers: &[Some(wgpu::VertexBufferLayout {
                        array_stride: GROUND_VERTEX_STRIDE,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &wgpu::vertex_attr_array![
                            0 => Float32x3, // position (km)
                            1 => Float32x2, // uv
                        ],
                    })],
                },
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    strip_index_format: None,
                    front_face: wgpu::FrontFace::Ccw,
                    cull_mode: None,
                    unclipped_depth: false,
                    polygon_mode: wgpu::PolygonMode::Fill,
                    conservative: false,
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    // The ground is the depth floor of the scene.
                    depth_write_enabled: Some(true),
                    depth_compare: Some(wgpu::CompareFunction::LessEqual),
                    stencil: wgpu::StencilState::default(),
                    bias: wgpu::DepthBiasState::default(),
                }),
                multisample: wgpu::MultisampleState {
                    count: 1,
                    mask: !0,
                    alpha_to_coverage_enabled: false,
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("gfs_main"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: COLOR_FORMAT,
                        blend: None, // opaque
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };

        let (job_tx, job_rx) = std::sync::mpsc::channel::<MeshJob>();
        let (done_tx, done_rx) = std::sync::mpsc::channel::<MeshDone>();
        std::thread::Builder::new()
            .name("stormscope-3dmesh".into())
            .spawn(move || {
                while let Ok(job) = job_rx.recv() {
                    let mesh = build_volume_mesh(&job.volume, &job.filter);
                    if done_tx.send(MeshDone { sig: job.sig, mesh }).is_err() {
                        break; // pane gone
                    }
                }
            })
            .expect("spawn 3d mesh thread");

        let empty = |label: &str, usage: wgpu::BufferUsages, size: u64| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size,
                usage: usage | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        };

        Self {
            ground_bgl,
            volume_pipeline,
            ground_pipeline,
            volume_ub,
            grid_ub,
            volume_bg,
            grid_bg,
            dummy_bg,
            ground_sampler,
            scene_sig: None,
            job_tx,
            done_rx,
            wanted: None,
            in_flight: None,
            vbuf: empty("stormscope.volume3d.vbuf", wgpu::BufferUsages::VERTEX, 4),
            ibuf: empty("stormscope.volume3d.ibuf", wgpu::BufferUsages::INDEX, 4),
            vbuf_cap: 4,
            ibuf_cap: 4,
            tilt_ranges: Vec::new(),
            grid_range: 0..0,
            tile_bgs: HashMap::new(),
            ground_vbuf: empty(
                "stormscope.volume3d.ground-vbuf",
                wgpu::BufferUsages::VERTEX,
                (GROUND_VERT_CAP * GROUND_VERTEX_STRIDE as usize) as u64,
            ),
            ground_ibuf: empty(
                "stormscope.volume3d.ground-ibuf",
                wgpu::BufferUsages::INDEX,
                (GROUND_INDEX_CAP * 4) as u64,
            ),
            ground_verts: Vec::new(),
            ground_indices: Vec::new(),
            ground_draws: Vec::new(),
            max_range_km: 0.0,
            tilt_count: 0,
            triangles: 0,
            vertex_count: 0,
            elev_lo_deg: 0.0,
            elev_hi_deg: 0.0,
            moment_seen: None,
            color: None,
            color_view: None,
            depth_view: None,
            size: [0, 0],
            tex_id: None,
            tex_dirty: false,
            scene_built_at: std::time::Instant::now(),
            last_key: None,
            last_full_key: None,
            last_change: std::time::Instant::now(),
            render_count: 0,
        }
    }

    /// Rebuild the scene synchronously (tests and one-off loads).
    /// Returns true when the GPU buffers were replaced.
    pub fn update_scene(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        volume: &Volume,
        filter: &MeshFilter,
        sig: u64,
    ) -> bool {
        if self.scene_sig == Some(sig) && self.moment_seen == Some(filter.moment) {
            return false;
        }
        let mesh = build_volume_mesh(volume, filter);
        self.upload_mesh(device, queue, mesh);
        self.scene_sig = Some(sig);
        self.moment_seen = Some(filter.moment);
        self.scene_built_at = std::time::Instant::now();
        // New geometry: the cached frame (if any) is stale.
        self.last_key = None;
        true
    }

    /// Ask the worker for `sig` (idempotent: the newest request wins and
    /// stale intermediate requests are never started).
    pub fn request_scene(&mut self, volume: std::sync::Arc<Volume>, filter: MeshFilter, sig: u64) {
        if self.scene_sig == Some(sig)
            || self.in_flight == Some(sig)
            || self.wanted.as_ref().map(|j| j.sig) == Some(sig)
        {
            return;
        }
        self.wanted = Some(MeshJob {
            sig,
            volume,
            filter,
        });
        self.start_wanted();
    }

    fn start_wanted(&mut self) {
        if self.in_flight.is_some() {
            return;
        }
        if let Some(job) = self.wanted.take() {
            self.in_flight = Some(job.sig);
            if self.job_tx.send(job).is_err() {
                self.in_flight = None;
            }
        }
    }

    /// Collect finished builds: the newest is uploaded into the (reused)
    /// buffers, stale ones are dropped — fast scrubbing skips intermediates
    /// instead of queueing them. Returns whether a scene was uploaded.
    pub fn pump_scene(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) -> bool {
        let mut uploaded = false;
        while let Ok(done) = self.done_rx.try_recv() {
            self.in_flight = None;
            let wanted_sig = self.wanted.as_ref().map(|j| j.sig);
            let stale = wanted_sig.is_some_and(|w| w != done.sig);
            if !stale {
                self.upload_mesh(device, queue, done.mesh);
                self.scene_sig = Some(done.sig);
                self.last_key = None;
                uploaded = true;
            }
            if wanted_sig == Some(done.sig) {
                self.wanted = None;
            }
            self.start_wanted();
        }
        uploaded
    }

    /// Upload a built mesh into the persistent (capacity-grown) buffers.
    fn upload_mesh(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, mesh: VolumeMesh) {
        let (mut grid_verts, grid_indices) = build_ground_grid(mesh.max_range_km.max(50.0));

        let volume_vertex_count = mesh.vertices.len() as u32;
        // Grid indices come after the cuts' indices and reference vertices
        // appended after the cuts' vertices.
        let mut indices = mesh.indices;
        let grid_range = {
            let start = indices.len() as u32;
            indices.extend(
                grid_indices
                    .iter()
                    .map(|i| i + volume_vertex_count)
                    .collect::<Vec<u32>>(),
            );
            start..indices.len() as u32
        };
        let mut vertices = mesh.vertices;
        vertices.append(&mut grid_verts);

        self.tilt_ranges = mesh.tilts.iter().map(|t| t.indices.clone()).collect();
        self.grid_range = grid_range;
        self.max_range_km = mesh.max_range_km;
        self.tilt_count = mesh.tilts.len();
        self.triangles = indices.len() / 3;
        self.vertex_count = vertices.len();
        self.elev_lo_deg = mesh.lowest_elev_deg;
        self.elev_hi_deg = mesh.highest_elev_deg;

        let vert_bytes: &[u8] = bytemuck::cast_slice(&vertices);
        let index_bytes: &[u8] = bytemuck::cast_slice(&indices);
        upload_into(
            device,
            queue,
            &mut self.vbuf,
            &mut self.vbuf_cap,
            "stormscope.volume3d.vbuf",
            wgpu::BufferUsages::VERTEX,
            vert_bytes,
        );
        upload_into(
            device,
            queue,
            &mut self.ibuf,
            &mut self.ibuf_cap,
            "stormscope.volume3d.ibuf",
            wgpu::BufferUsages::INDEX,
            index_bytes,
        );
    }

    /// Whether any scene has been built yet.
    pub fn has_scene(&self) -> bool {
        self.scene_sig.is_some()
    }

    /// Set the ground quads for this frame: `(egui texture, [x0, y0, x1, y1])`
    /// in scene-local km. Resolves each egui texture to a cached wgpu bind
    /// group (the tile layer uploaded the pixels; the GPU texture is shared,
    /// not copied) and rewrites the small quad geometry.
    pub fn set_ground_tiles(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        render_state: &egui_wgpu::RenderState,
        tiles: &[(egui::TextureId, [f32; 4])],
    ) {
        self.ground_verts.clear();
        self.ground_indices.clear();
        self.ground_draws.clear();

        for (id, b) in tiles {
            if self.ground_verts.len() + 4 > GROUND_VERT_CAP
                || self.ground_indices.len() + 6 > GROUND_INDEX_CAP
            {
                break;
            }
            if !self.tile_bgs.contains_key(id) {
                // The egui renderer owns the tile texture; borrow its wgpu
                // handle long enough to build our own bind group around it.
                let guard = render_state.renderer.read();
                let Some(tex) = guard.texture(id) else {
                    continue;
                };
                let Some(wgpu_tex) = tex.texture.as_ref() else {
                    continue;
                };
                let view = wgpu_tex.create_view(&wgpu::TextureViewDescriptor::default());
                let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("stormscope.volume3d.tile-bg"),
                    layout: &self.ground_bgl,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(&view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::Sampler(&self.ground_sampler),
                        },
                    ],
                });
                drop(guard);
                self.tile_bgs.insert(*id, bg);
            }
            let (x0, y0, x1, y1) = (b[0], b[1], b[2], b[3]);
            let base = self.ground_verts.len() as u32;
            // Mercator y grows southward; v = 0 is the tile's north edge.
            self.ground_verts.push(GroundVertex {
                pos_km: [x0, y0, 0.0],
                uv: [0.0, 1.0],
            });
            self.ground_verts.push(GroundVertex {
                pos_km: [x1, y0, 0.0],
                uv: [1.0, 1.0],
            });
            self.ground_verts.push(GroundVertex {
                pos_km: [x1, y1, 0.0],
                uv: [1.0, 0.0],
            });
            self.ground_verts.push(GroundVertex {
                pos_km: [x0, y1, 0.0],
                uv: [0.0, 0.0],
            });
            let start = self.ground_indices.len() as u32;
            self.ground_indices.extend_from_slice(&[
                base,
                base + 1,
                base + 2,
                base,
                base + 2,
                base + 3,
            ]);
            self.ground_draws
                .push((*id, start..self.ground_indices.len() as u32));
        }

        if !self.ground_verts.is_empty() {
            queue.write_buffer(
                &self.ground_vbuf,
                0,
                bytemuck::cast_slice(&self.ground_verts),
            );
            queue.write_buffer(
                &self.ground_ibuf,
                0,
                bytemuck::cast_slice(&self.ground_indices),
            );
        }
        // Keep the bind-group cache bounded: past the cap, hold only what
        // this frame draws (the rest re-registers in one bind group).
        if self.tile_bgs.len() > TILE_BG_CAP {
            let keep: std::collections::HashSet<egui::TextureId> =
                tiles.iter().map(|(id, _)| *id).collect();
            self.tile_bgs.retain(|id, _| keep.contains(id));
        }
    }

    pub fn max_range_km(&self) -> f32 {
        self.max_range_km
    }
    pub fn tilt_count(&self) -> usize {
        self.tilt_count
    }
    pub fn triangles(&self) -> usize {
        self.triangles
    }
    pub fn vertex_count(&self) -> usize {
        self.vertex_count
    }
    pub fn elev_lo_deg(&self) -> f32 {
        self.elev_lo_deg
    }
    pub fn elev_hi_deg(&self) -> f32 {
        self.elev_hi_deg
    }

    fn ensure_targets(&mut self, device: &wgpu::Device, size: [u32; 2]) {
        if self.size == size && self.color.is_some() {
            return;
        }
        let extent = wgpu::Extent3d {
            width: size[0].max(16),
            height: size[1].max(16),
            depth_or_array_layers: 1,
        };
        let color = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("stormscope.volume3d.color"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: COLOR_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC, // snapshot tooling (smoke test)
            view_formats: &[],
        });
        let depth = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("stormscope.volume3d.depth"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: DEPTH_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        self.color_view = Some(color.create_view(&wgpu::TextureViewDescriptor::default()));
        self.depth_view = Some(depth.create_view(&wgpu::TextureViewDescriptor::default()));
        self.color = Some(color);
        self.size = size;
        self.tex_dirty = true;
    }

    /// Render one frame of the scene into the offscreen texture.
    ///
    /// `alt_min_km`/`alt_max_km` are the altitude slab in kilometres (the UI
    /// edits feet); the grid and ground are exempt from it.
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        size: [u32; 2],
        camera: &Camera,
        vertical_exaggeration: f32,
        opacity: f32,
        threshold: f32,
        alt_min_km: f32,
        alt_max_km: f32,
        min_half_render_px: f32,
    ) {
        self.ensure_targets(device, size);
        self.render_count += 1;
        let aspect = size[0] as f32 / size[1].max(1) as f32;
        let view_proj = camera.view_proj(aspect, self.max_range_km.max(1.0));
        let eye = camera.eye();
        let viewport_h = size[1].max(1) as f32;
        let volume_uniforms = Uniforms {
            view_proj,
            params: [vertical_exaggeration, opacity, threshold, 0.0],
            params2: [alt_min_km, alt_max_km, min_half_render_px, 0.0],
            params3: [eye[0], eye[1], eye[2], viewport_h],
        };
        // Grid/ground: NO_THRESHOLD_VALUE passes any threshold, the altitude
        // slab is opened wide, and the line width is the screen-space
        // minimum expressed in this pass's pixels.
        let grid_uniforms = Uniforms {
            view_proj,
            params: [vertical_exaggeration, 1.0, threshold, 0.0],
            params2: [0.0, 1.0e9, min_half_render_px, 0.0],
            params3: [eye[0], eye[1], eye[2], viewport_h],
        };
        queue.write_buffer(&self.volume_ub, 0, bytemuck::bytes_of(&volume_uniforms));
        queue.write_buffer(&self.grid_ub, 0, bytemuck::bytes_of(&grid_uniforms));

        let (Some(color_view), Some(depth_view)) = (&self.color_view, &self.depth_view) else {
            return;
        };
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("stormscope.volume3d.encoder"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("stormscope.volume3d.pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: color_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        // Pure black: anything the ground tiles do not
                        // cover should be void, not blue-gray.
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });

            // 1) Basemap ground (opaque, depth writes on).
            if !self.ground_draws.is_empty() {
                pass.set_pipeline(&self.ground_pipeline);
                pass.set_bind_group(0, &self.grid_bg, &[]);
                pass.set_vertex_buffer(0, self.ground_vbuf.slice(..));
                pass.set_index_buffer(self.ground_ibuf.slice(..), wgpu::IndexFormat::Uint32);
                for (id, range) in &self.ground_draws {
                    if let Some(bg) = self.tile_bgs.get(id) {
                        pass.set_bind_group(1, bg, &[]);
                        pass.draw_indexed(range.clone(), 0, 0..1);
                    }
                }
            }

            // 2) Range grid + 3) volume, sharing the camera/volume pipeline.
            pass.set_pipeline(&self.volume_pipeline);
            pass.set_bind_group(1, &self.dummy_bg, &[]);
            pass.set_vertex_buffer(0, self.vbuf.slice(..));
            pass.set_index_buffer(self.ibuf.slice(..), wgpu::IndexFormat::Uint32);
            if self.grid_range.start < self.grid_range.end {
                pass.set_bind_group(0, &self.grid_bg, &[]);
                pass.draw_indexed(self.grid_range.clone(), 0, 0..1);
            }
            if !self.tilt_ranges.is_empty() {
                pass.set_bind_group(0, &self.volume_bg, &[]);
                for range in &self.tilt_ranges {
                    pass.draw_indexed(range.clone(), 0, 0..1);
                }
            }
        }
        queue.submit(Some(encoder.finish()));
    }

    /// Render only what changed: skip entirely when nothing did, render at
    /// half resolution while the camera/settings are still moving, and land
    /// one full-resolution frame ~160 ms after the motion stops.
    ///
    /// This is the pane's answer to being GPU-bound on a 4K desktop: an
    /// interaction burst paints 4x fewer pixels, a settled view is sharp,
    /// and idle UI repaints (the app wakes about once a second for its
    /// timers) cost a texture sample instead of a full volume pass.
    #[allow(clippy::too_many_arguments)]
    pub fn render_if_needed(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        screen_px: [u32; 2],
        camera: &Camera,
        vertical_exaggeration: f32,
        opacity: f32,
        threshold: f32,
        alt_min_km: f32,
        alt_max_km: f32,
        ground_key: u64,
        min_half_screen_px: f32,
    ) {
        use std::hash::{Hash, Hasher};
        let key = {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            for v in [
                camera.yaw_deg,
                camera.pitch_deg,
                camera.distance_km,
                camera.target_km[0],
                camera.target_km[1],
                camera.target_km[2],
                vertical_exaggeration,
                opacity,
                threshold,
                alt_min_km,
                alt_max_km,
            ] {
                v.to_bits().hash(&mut h);
            }
            screen_px.hash(&mut h);
            self.scene_sig.hash(&mut h);
            ground_key.hash(&mut h);
            h.finish()
        };
        let since_change_ms = self.last_change.elapsed().as_millis() as u64;
        match render_decision(self.last_key, self.last_full_key, key, since_change_ms) {
            RenderDecision::Skip => {}
            RenderDecision::Full => {
                if self.color.is_some() {
                    // Settled: supersample — AA for edges and tiles alike.
                    let size = [
                        ((screen_px[0] as f32) * SUPERSAMPLE).round() as u32,
                        ((screen_px[1] as f32) * SUPERSAMPLE).round() as u32,
                    ];
                    self.render(
                        device,
                        queue,
                        size,
                        camera,
                        vertical_exaggeration,
                        opacity,
                        threshold,
                        alt_min_km,
                        alt_max_km,
                        min_half_screen_px * SUPERSAMPLE,
                    );
                    self.last_full_key = Some(key);
                }
            }
            RenderDecision::Interactive => {
                self.last_key = Some(key);
                self.last_change = std::time::Instant::now();
                // Moving: exact display resolution. Never *below* it — sub-
                // pixel lines (grid rings, spokes) flicker as coverage flips
                // frame to frame, which is exactly when the user is looking.
                let size = [(screen_px[0]).max(64), (screen_px[1]).max(64)];
                self.render(
                    device,
                    queue,
                    size,
                    camera,
                    vertical_exaggeration,
                    opacity,
                    threshold,
                    alt_min_km,
                    alt_max_km,
                    min_half_screen_px,
                );
            }
        }
    }

    /// The offscreen color target (created by the first [`Self::render`]).
    /// Exposed for snapshot tooling — the headless smoke test reads it back
    /// to a PNG.
    pub fn color_texture(&self) -> Option<&wgpu::Texture> {
        self.color.as_ref()
    }

    /// Egui texture id of the current frame, registering or rebinding after
    /// target recreations. Call after [`Self::render`].
    pub fn texture_id(&mut self, render_state: &egui_wgpu::RenderState) -> egui::TextureId {
        let view = self.color_view.as_ref().expect("render() ran first");
        match self.tex_id {
            Some(id) => {
                if self.tex_dirty {
                    render_state
                        .renderer
                        .write()
                        .update_egui_texture_from_wgpu_texture(
                            &render_state.device,
                            view,
                            wgpu::FilterMode::Linear,
                            id,
                        );
                    self.tex_dirty = false;
                }
                id
            }
            None => {
                let id = render_state.renderer.write().register_native_texture(
                    &render_state.device,
                    view,
                    wgpu::FilterMode::Linear,
                );
                self.tex_dirty = false;
                self.tex_id = Some(id);
                id
            }
        }
    }
}

/// Display-space shading: colors arrive premultiplied from
/// [`radar_render::volume3d`]; the fragment shader only darkens by a
/// hemisphere-ish light so the folds of the volume read, multiplies the
/// user's opacity in, and discards outside the echo/altitude window.
/// Vertical exaggeration happens here too, so its slider is free as well.
/// The ground entries sample the basemap tiles with no processing at all —
/// the ground is the map.
const SHADER: &str = r#"
struct Cam {
    view_proj: mat4x4<f32>,
    params: vec4<f32>,   // x: vertical exaggeration, y: opacity, z: threshold
    params2: vec4<f32>,  // x/y: altitude slab km (min/max)
    params3: vec4<f32>,  // xyz: eye km, w: viewport height px
};
@group(0) @binding(0) var<uniform> cam: Cam;
@group(1) @binding(0) var ground_tex: texture_2d<f32>;
@group(1) @binding(1) var ground_samp: sampler;

struct VsIn {
    @location(0) pos: vec3<f32>,
    @location(1) nrm: vec3<f32>,
    @location(2) col: vec4<f32>,
    @location(3) value: f32,
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) nrm: vec3<f32>,
    @location(1) col: vec4<f32>,
    @location(2) value: f32,
    @location(3) alt: f32,
};

@vertex
fn vs_main(in: VsIn) -> VsOut {
    var out: VsOut;
    var p = vec3<f32>(in.pos.x, in.pos.y, in.pos.z * cam.params.x);
    // Ground-grid ribbons (tagged with NO_THRESHOLD_VALUE) arrive as
    // centerlines with their perpendicular in the normal: give them a
    // screen-space minimum width so rings and spokes cannot alias down to
    // sub-pixel flicker at distance or steep pitch.
    if (in.value > 1.0e29) {
        let perp = normalize(in.nrm.xy);
        let dist = length(cam.params3.xy - in.pos.xy);
        // World km covered by one pixel at that distance (fov 40 deg).
        let px_world = 2.0 * 0.36397023 * dist / max(cam.params3.w, 1.0);
        let half = max(cam.params2.z * px_world, 0.10);
        p = vec3<f32>(p.xy + perp * half, p.z);
    }
    out.clip = cam.view_proj * vec4<f32>(p, 1.0);
    out.nrm = in.nrm;
    out.col = in.col;
    out.value = in.value;
    out.alt = in.pos.z; // physical km; the slab is a display filter
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    if (in.value < cam.params.z) {
        discard;
    }
    if (in.alt < cam.params2.x || in.alt > cam.params2.y) {
        discard;
    }
    let a = clamp(in.col.a * cam.params.y, 0.0, 1.0);
    var shade = 0.62 + 0.38 * abs(dot(normalize(in.nrm), normalize(vec3<f32>(0.40, -0.55, 0.75))));
    if (in.value > 1.0e29) {
        // The grid's normals carry the ribbon perpendicular (not a surface
        // normal), so shade it flat.
        shade = 0.95;
    }
    return vec4<f32>(in.col.rgb * shade * cam.params.y, a);
}

struct GroundIn {
    @location(0) pos: vec3<f32>,
    @location(1) uv: vec2<f32>,
};

struct GroundOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn gvs_main(in: GroundIn) -> GroundOut {
    var out: GroundOut;
    out.clip = cam.view_proj * vec4<f32>(in.pos, 1.0);
    out.uv = in.uv;
    return out;
}

@fragment
fn gfs_main(in: GroundOut) -> @location(0) vec4<f32> {
    // Mip emulation for the basemap tiles. egui's tile textures are single
    // mip, so when the ground compresses at a steep or grazing pitch one
    // pixel covers many texels and the one-tap sample picks effectively
    // random ones — roads and labels crawl and flicker while the camera
    // moves. Measure the texel footprint from screen-space derivatives and
    // blend a small diagonal cross of taps in as the footprint grows (a
    // box-ish filter, so the texture stays sharp at 1:1). All taps are
    // taken unconditionally: WGSL only allows textureSample in uniform
    // control flow.
    let dx = dpdx(in.uv);
    let dy = dpdy(in.uv);
    let footprint = max(length(dx), length(dy)) * 256.0; // texels per pixel
    let a = textureSample(ground_tex, ground_samp, in.uv + (dx + dy) * 0.30)
        + textureSample(ground_tex, ground_samp, in.uv - (dx + dy) * 0.30)
        + textureSample(ground_tex, ground_samp, in.uv + (dx - dy) * 0.30)
        + textureSample(ground_tex, ground_samp, in.uv - (dx - dy) * 0.30);
    let base = textureSample(ground_tex, ground_samp, in.uv);
    let blur = clamp((footprint - 1.1) / 6.0, 0.0, 0.9);
    let c = mix(base, a * 0.25, blur);
    return vec4<f32>(c.rgb, 1.0);
}
"#;

/// One outstanding mesh build.
struct MeshJob {
    sig: u64,
    volume: std::sync::Arc<Volume>,
    filter: MeshFilter,
}

/// A finished mesh on its way back to the UI thread.
struct MeshDone {
    sig: u64,
    mesh: VolumeMesh,
}

/// Write `bytes` into `buf`, growing the allocation (to 1.5x what is
/// needed, so same-shaped scans reuse it) only when it no longer fits.
/// Playback rebuilds the scene on every tick; re-allocating ~20 MB of
/// buffers eight times a second is churn nobody needs.
fn upload_into(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    buf: &mut wgpu::Buffer,
    cap: &mut u64,
    label: &str,
    usage: wgpu::BufferUsages,
    bytes: &[u8],
) {
    let need = bytes.len() as u64;
    if need > *cap {
        let size = ((need.max(*cap * 3 / 2)) + 3) & !3;
        *buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size,
            usage: usage | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        *cap = size;
    }
    if !bytes.is_empty() {
        queue.write_buffer(buf, 0, bytes);
    }
}

/// Multiplier the settled frame renders at (motion frames render at 1x).
const SUPERSAMPLE: f32 = 1.6;

/// How long after the last change the full-resolution frame lands.
const SETTLE_MS: u64 = 160;

#[derive(Debug, PartialEq, Eq)]
enum RenderDecision {
    /// Nothing changed (and the full-res frame is already on screen).
    Skip,
    /// Something changed: draw this frame at half resolution.
    Interactive,
    /// Settled: draw one sharp frame and stop.
    Full,
}

/// The pane's frame policy, split out so it is testable without a GPU:
/// redraw on every change (half res), land one full-res frame shortly after
/// the motion stops, skip everything else. Idle UI repaints then cost one
/// texture sample instead of a volume pass.
fn render_decision(
    last_key: Option<u64>,
    last_full_key: Option<u64>,
    key: u64,
    since_change_ms: u64,
) -> RenderDecision {
    if last_key == Some(key) {
        if last_full_key != Some(key) && since_change_ms >= SETTLE_MS {
            RenderDecision::Full
        } else {
            RenderDecision::Skip
        }
    } else {
        RenderDecision::Interactive
    }
}

#[cfg(test)]
mod tests {
    use super::{RenderDecision, SETTLE_MS, render_decision};

    #[test]
    fn first_frame_is_interactive() {
        assert_eq!(
            render_decision(None, None, 7, u64::MAX),
            RenderDecision::Interactive
        );
    }

    #[test]
    fn same_key_skips_until_settled_then_lands_one_full_frame() {
        // Just changed (key 7 is current, no full-res frame yet).
        assert_eq!(
            render_decision(Some(7), None, 7, 0),
            RenderDecision::Skip,
            "no redraw while mid-motion"
        );
        assert_eq!(
            render_decision(Some(7), None, 7, SETTLE_MS - 1),
            RenderDecision::Skip,
            "still inside the settle window"
        );
        assert_eq!(
            render_decision(Some(7), None, 7, SETTLE_MS),
            RenderDecision::Full,
            "settle: one sharp frame"
        );
        // Full frame delivered: back to skipping.
        assert_eq!(
            render_decision(Some(7), Some(7), 7, SETTLE_MS * 10),
            RenderDecision::Skip
        );
    }

    #[test]
    fn a_new_key_is_always_an_interactive_frame() {
        assert_eq!(
            render_decision(Some(7), Some(7), 8, 5),
            RenderDecision::Interactive
        );
        // Even right after a full-res frame: a new change downgrades to
        // half-res first (the settle pass will sharpen it).
        assert_eq!(
            render_decision(Some(7), Some(7), 8, 0),
            RenderDecision::Interactive
        );
    }
}
