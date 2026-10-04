//! Instanced sprite pipeline (AR-6): premultiplied alpha, nearest sampling,
//! z-sorted [`DrawList`] batched into per-page instance runs.
//!
//! One static unit-quad vertex buffer + one growable per-frame instance
//! buffer. Small textures (≤ [`MAX_ATLAS_DIM`] px) are shelf-packed into
//! shared 1024×1024 atlas **pages**; larger ones (512×512 light cookies,
//! the 1920×1080 title, PR-11) get standalone pages. Either way a page is
//! one bind group, and the draw walks the pre-sorted list batching adjacent
//! same-page instances — atlasing keeps the common case at one draw call.
//!
//! Atlas placement is invisible to the game: instances address sprites by
//! asset [`Handle`] + pixel `src` rect; [`SpritePass::upload`] records the
//! page + pixel offset and UVs are adjusted at draw.
//!
//! **Normal maps (R-6)**: a texture may register a companion normal map
//! ([`SpritePass::upload_normal`]) which is written into the page's parallel
//! **normal page** at the *same placement* as the diffuse — so a sprite's
//! normal shares its UVs and no extra per-instance rect is needed. Normal
//! pages are linear (`Rgba8Unorm` — normal data must not be sRGB-decoded)
//! and bound alongside the diffuse (a shared 1×1 dummy where a page has no
//! normals). Instances flag `has_normal` only when their `normal` handle
//! matches the registered companion.
//!
//! **Two pipelines** over the same shader/vertex state (R-6):
//! - the **scene** pipeline (world channel): MRT — albedo + screen-space
//!   normal + light-mask targets (see `sprite.wgsl` `fs_scene`);
//! - the **screen** pipeline (UI/HUD channel): single target, drawn after
//!   the lighting composite so it bypasses lighting entirely.
//!
//! **Color space** (R-13 parity, D-11): Godot blends all 2D in sRGB
//! ("gamma") space, so by default this pipeline does too — pages upload as
//! plain `Rgba8Unorm` (sampled raw, no sRGB decode), modulate colors pass
//! through untouched (Godot multiplies modulates in sRGB), and all blending
//! happens on gamma-space values in the scene target (the swapchain blit is
//! the only place any transfer-function conversion occurs — see
//! `render::blit`). Under [`LightingSpace::Linear`] the **scene** pipeline
//! instead uses the `fs_scene_linear` entry point, which decodes the page
//! texel sRGB → linear on sample and blends into a half-float linear albedo;
//! the modulate is then taken as linear. The screen pipeline is the same in
//! both modes (the UI stays gamma), and pages upload identically.

use std::collections::HashMap;

use glam::{Affine2, Mat4, Vec2};
use wgpu::util::DeviceExt;

use crate::assets::{Handle, Texture};
use crate::canvas::LightingSpace;
use crate::canvas::atlas::ShelfPacker;
use crate::canvas::camera::WorldUnits;
use crate::canvas::draw::{DrawList, Rect, SpriteInstance};
use crate::canvas::light::mask_bits;

/// Textures with both dimensions at or under this many pixels go into shared
/// atlas pages; anything larger stays standalone (AR-6: the game's sprites
/// are ≤ 384 px; the 512×512 lights and 1920×1080 title are not atlased).
pub const MAX_ATLAS_DIM: u32 = 384;

/// Atlas page size in pixels (square). Holds every small sprite of the game
/// with room to spare; more pages open if one fills.
pub const ATLAS_PAGE_SIZE: u32 = 1024;

/// The scene pass's screen-space **normal** target format. Linear (normals
/// are data, not color); rgb = encoded normal premultiplied by sprite alpha.
pub const NORMAL_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// The scene pass's **light-mask** target format: rgb = light-layer coverage
/// (layers 1/2/4), a = normal-map weight.
pub const MASK_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// Per-instance GPU data. Matches `InstanceInput` in `sprite.wgsl`.
///
/// `model_*` + `translation` pack a 2×3 affine (rotation·size columns +
/// translation, in the channel's camera coordinates — see
/// [`sprite_affine`]). UVs are normalized page coords with flips already
/// folded in (swapped endpoints).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct InstanceRaw {
    model_x: [f32; 2],
    model_y: [f32; 2],
    translation: [f32; 2],
    uv_min: [f32; 2],
    uv_max: [f32; 2],
    color: [f32; 4],
    /// rgb = light-mask layer bits, w = has-normal-map flag (R-6).
    misc: [f32; 4],
    // Pad the stride to 16-byte alignment.
    _pad: [f32; 2],
}

/// One GPU texture page (a shared atlas or a standalone texture): its
/// diffuse texture + optional companion normal page, the bind group, and
/// its pixel size (for UV normalization).
struct Page {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    /// Companion normal page (same size/placement as the diffuse), created
    /// lazily on the first normal upload targeting this page.
    normal: Option<wgpu::Texture>,
    bind_group: wgpu::BindGroup,
    size: Vec2,
}

/// An atlas page's packer state (indexes into `pages`).
struct AtlasPage {
    page: usize,
    packer: ShelfPacker,
}

/// Where a texture handle's pixels live: which page, at what pixel offset.
struct Entry {
    page: usize,
    offset: Vec2,
    /// The source texture's own size in pixels (full-texture src default).
    size: Vec2,
    /// The registered companion normal-map handle, if any (R-6).
    normal: Option<Handle<Texture>>,
}

/// A batched draw run: `count` instances starting at `first`, all on `page`,
/// through camera `camera` (0 = world, 1 = screen), scissored to `scissor`
/// (target pixels; `None` = the full target).
struct Batch {
    camera: usize,
    page: usize,
    first: u32,
    count: u32,
    scissor: Option<[u32; 4]>,
}

/// Clamp a logical-pixel clip rect to a `target`-sized render target,
/// returning integer `[x, y, w, h]` scissor bounds — or `None` when the
/// clip covers nothing (fully off-target or empty). Pure; unit-tested.
pub fn scissor_px(clip: &Rect, target: (u32, u32)) -> Option<[u32; 4]> {
    let x0 = clip.min.x.floor().max(0.0) as u32;
    let y0 = clip.min.y.floor().max(0.0) as u32;
    let x1 = (clip.max.x.ceil().max(0.0) as u32).min(target.0);
    let y1 = (clip.max.y.ceil().max(0.0) as u32).min(target.1);
    if x0 >= x1 || y0 >= y1 {
        None
    } else {
        Some([x0, y0, x1 - x0, y1 - y0])
    }
}

/// One camera slot: uniform buffer + bind group.
struct CameraSlot {
    buffer: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
}

/// The instanced sprite pipelines + texture pages + instance buffer.
pub struct SpritePass {
    /// World-channel MRT pipeline (albedo + normal + mask).
    scene_pipeline: wgpu::RenderPipeline,
    /// Screen-channel single-target pipeline (post-composite, unlit).
    screen_pipeline: wgpu::RenderPipeline,
    quad_vb: wgpu::Buffer,
    instance_buffer: wgpu::Buffer,
    instance_capacity: u64,
    /// `[world, screen]` camera uniforms (group 0).
    cameras: [CameraSlot; 2],
    texture_bgl: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    /// Shared 1×1 dummy normal view for pages without normal maps.
    default_normal_view: wgpu::TextureView,
    pages: Vec<Page>,
    atlases: Vec<AtlasPage>,
    entries: HashMap<Handle<Texture>, Entry>,
    /// Scratch reused every frame.
    raw: Vec<InstanceRaw>,
    batches: Vec<Batch>,
    /// Render-target size in pixels (set by `prepare`; used to reset the
    /// scissor for unclipped batches).
    target_size: (u32, u32),
}

/// The static unit quad: two triangles spanning `[0,1]²`.
const QUAD_VERTS: &[[f32; 2]] = &[
    [0.0, 0.0],
    [1.0, 0.0],
    [1.0, 1.0],
    [0.0, 0.0],
    [1.0, 1.0],
    [0.0, 1.0],
];

const INITIAL_INSTANCE_CAP: u64 = 256;

impl SpritePass {
    /// Build the pipelines: the scene MRT pipeline targeting
    /// `[albedo_format, NORMAL_FORMAT, MASK_FORMAT]` with the `space`'s
    /// fragment entry point, and the screen pipeline targeting
    /// `screen_format` alone (the composited target's format —
    /// `render::blit::SCENE_FORMAT`). `queue` seeds the shared 1×1 default
    /// normal texture.
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        albedo_format: wgpu::TextureFormat,
        screen_format: wgpu::TextureFormat,
        space: LightingSpace,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("sprite shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("sprite.wgsl").into()),
        });

        // --- Camera uniforms (group 0): world + screen slots.
        let camera_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("sprite camera bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let camera_slot = |label: &str| {
            let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: size_of::<[[f32; 4]; 4]>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(label),
                layout: &camera_bgl,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: buffer.as_entire_binding(),
                }],
            });
            CameraSlot { buffer, bind_group }
        };
        let cameras = [camera_slot("world camera"), camera_slot("screen camera")];

        // --- Texture page + sampler + companion normal page (group 1).
        let texture_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("sprite page bgl"),
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
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });

        // Nearest sampling for crisp pixel art (AR-6, PR: no mipmaps).
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("sprite sampler (nearest)"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });

        // Shared dummy normal (flat +Z) for pages without normal maps; only
        // ever sampled with a zero has-normal flag.
        let default_normal = device.create_texture_with_data(
            queue,
            &wgpu::TextureDescriptor {
                label: Some("default normal (1x1 flat)"),
                size: wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: NORMAL_FORMAT,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            },
            wgpu::util::TextureDataOrder::LayerMajor,
            &[128, 128, 255, 255],
        );
        let default_normal_view =
            default_normal.create_view(&wgpu::TextureViewDescriptor::default());

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("sprite pipeline layout"),
            bind_group_layouts: &[Some(&camera_bgl), Some(&texture_bgl)],
            immediate_size: 0,
        });

        let quad_vb_layout = wgpu::VertexBufferLayout {
            array_stride: size_of::<[f32; 2]>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &wgpu::vertex_attr_array![0 => Float32x2],
        };
        let instance_vb_layout = wgpu::VertexBufferLayout {
            array_stride: size_of::<InstanceRaw>() as u64,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &wgpu::vertex_attr_array![
                1 => Float32x2, // model_x
                2 => Float32x2, // model_y
                3 => Float32x2, // translation
                4 => Float32x2, // uv_min
                5 => Float32x2, // uv_max
                6 => Float32x4, // color
                7 => Float32x4, // misc (mask bits + has_normal)
            ],
        };

        // Premultiplied-alpha blend (AR-6); the MRT targets premultiply their
        // payloads in-shader too, so one blend state serves all targets.
        let premul = wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
            alpha: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
        };
        let target = |format: wgpu::TextureFormat| {
            Some(wgpu::ColorTargetState {
                format,
                blend: Some(premul),
                write_mask: wgpu::ColorWrites::ALL,
            })
        };

        let make_pipeline =
            |label: &str, entry: &str, targets: &[Option<wgpu::ColorTargetState>]| {
                device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some(label),
                    layout: Some(&layout),
                    vertex: wgpu::VertexState {
                        module: &shader,
                        entry_point: Some("vs_main"),
                        buffers: &[quad_vb_layout.clone(), instance_vb_layout.clone()],
                        compilation_options: Default::default(),
                    },
                    fragment: Some(wgpu::FragmentState {
                        module: &shader,
                        entry_point: Some(entry),
                        targets,
                        compilation_options: Default::default(),
                    }),
                    primitive: wgpu::PrimitiveState {
                        topology: wgpu::PrimitiveTopology::TriangleList,
                        ..Default::default()
                    },
                    // No depth buffer: painter's order via the z-sorted DrawList.
                    depth_stencil: None,
                    multisample: wgpu::MultisampleState::default(),
                    multiview_mask: None,
                    cache: None,
                })
            };
        let scene_pipeline = make_pipeline(
            "sprite scene pipeline (MRT)",
            match space {
                LightingSpace::Gamma => "fs_scene",
                LightingSpace::Linear => "fs_scene_linear",
            },
            &[
                target(albedo_format),
                target(NORMAL_FORMAT),
                target(MASK_FORMAT),
            ],
        );
        let screen_pipeline = make_pipeline(
            "sprite screen pipeline",
            "fs_main",
            &[target(screen_format)],
        );

        let quad_vb = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("unit quad VB"),
            contents: bytemuck::cast_slice(QUAD_VERTS),
            usage: wgpu::BufferUsages::VERTEX,
        });

        let instance_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("sprite instance buffer"),
            size: INITIAL_INSTANCE_CAP * size_of::<InstanceRaw>() as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            scene_pipeline,
            screen_pipeline,
            quad_vb,
            instance_buffer,
            instance_capacity: INITIAL_INSTANCE_CAP,
            cameras,
            texture_bgl,
            sampler,
            default_normal_view,
            pages: Vec::new(),
            atlases: Vec::new(),
            entries: HashMap::new(),
            raw: Vec::new(),
            batches: Vec::new(),
            target_size: (0, 0),
        }
    }

    /// Whether `handle` has been uploaded already.
    pub fn has_texture(&self, handle: Handle<Texture>) -> bool {
        self.entries.contains_key(&handle)
    }

    /// Upload a decoded texture to the GPU under its asset `handle`:
    /// shelf-packed into a shared atlas page when small (≤ [`MAX_ATLAS_DIM`]),
    /// standalone otherwise. Idempotent per handle.
    pub fn upload(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        handle: Handle<Texture>,
        tex: &Texture,
    ) {
        if self.entries.contains_key(&handle) {
            return;
        }
        let entry = self.place(device, queue, tex);
        self.entries.insert(handle, entry);
    }

    /// Replace a texture's GPU pixels without changing its asset `handle`.
    /// The handle may identify an ordinary diffuse upload, a normal map
    /// registered to one or more diffuse textures, or both. Returns `false`
    /// when the handle is unknown in either role.
    ///
    /// Same-size replacements write into the existing placement and preserve
    /// any registered normal-map association. A size change relocates the
    /// texture because atlas placements have fixed extents, and clears that
    /// association: the caller must register a matching normal map again.
    /// Normal-only handles update every compatible diffuse association; a
    /// mismatch detaches only the incompatible association.
    pub(crate) fn replace(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        handle: Handle<Texture>,
        tex: &Texture,
    ) -> bool {
        let size = Vec2::new(tex.width as f32, tex.height as f32);
        let mut found = false;

        if let Some(entry) = self.entries.get(&handle) {
            found = true;
            if entry.size == size {
                let (x, y) = (entry.offset.x as u32, entry.offset.y as u32);
                write_pixels(queue, &self.pages[entry.page].texture, x, y, tex);
            } else {
                let entry = self.place(device, queue, tex);
                self.entries.insert(handle, entry);
            }
        }

        // Snapshot the owners first: creating a page's lazy normal texture
        // mutates `self`, and one normal handle may serve several diffuses.
        let owners: Vec<_> = self
            .entries
            .iter()
            .filter_map(|(diffuse, entry)| {
                (entry.normal == Some(handle)).then_some((
                    *diffuse,
                    entry.page,
                    entry.offset,
                    entry.size,
                ))
            })
            .collect();
        found |= !owners.is_empty();
        for (diffuse, page, offset, diffuse_size) in owners {
            if diffuse_size != size {
                self.entries
                    .get_mut(&diffuse)
                    .expect("normal owner came from entries")
                    .normal = None;
                continue;
            }
            self.ensure_page_normal(device, page);
            let normal = self.pages[page]
                .normal
                .as_ref()
                .expect("ensure_page_normal just created it");
            write_pixels(queue, normal, offset.x as u32, offset.y as u32, tex);
        }
        found
    }

    /// Allocate a GPU placement for `tex`, upload its pixels, and return the
    /// unassociated entry. Existing handle ownership is managed by callers.
    fn place(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, tex: &Texture) -> Entry {
        let (w, h) = (tex.width, tex.height);
        if w <= MAX_ATLAS_DIM && h <= MAX_ATLAS_DIM {
            let (page, x, y) = self.atlas_spot(device, w, h);
            write_pixels(queue, &self.pages[page].texture, x, y, tex);
            Entry {
                page,
                offset: Vec2::new(x as f32, y as f32),
                size: Vec2::new(w as f32, h as f32),
                normal: None,
            }
        } else {
            let page = self.create_page(device, w, h, "standalone sprite page");
            write_pixels(queue, &self.pages[page].texture, 0, 0, tex);
            Entry {
                page,
                offset: Vec2::ZERO,
                size: Vec2::new(w as f32, h as f32),
                normal: None,
            }
        }
    }

    /// Register `normal_handle`/`tex` as the companion normal map of the
    /// already-uploaded `diffuse` texture (R-6): the normal pixels are
    /// written into the page's normal companion at the diffuse's placement,
    /// so instances sample it with the same UVs. The normal texture must
    /// match the diffuse's dimensions (mismatches are skipped — a content
    /// error, not a crash). Idempotent per diffuse.
    pub fn upload_normal(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        diffuse: Handle<Texture>,
        normal_handle: Handle<Texture>,
        tex: &Texture,
    ) {
        let Some(entry) = self.entries.get(&diffuse) else {
            debug_assert!(false, "upload_normal before upload of the diffuse");
            return;
        };
        if entry.normal.is_some() {
            return;
        }
        if entry.size != Vec2::new(tex.width as f32, tex.height as f32) {
            debug_assert!(false, "normal map size differs from its diffuse");
            return;
        }
        let page = entry.page;
        let (x, y) = (entry.offset.x as u32, entry.offset.y as u32);
        self.ensure_page_normal(device, page);
        let normal_tex = self.pages[page]
            .normal
            .as_ref()
            .expect("ensure_page_normal just created it");
        write_pixels(queue, normal_tex, x, y, tex);
        self.entries
            .get_mut(&diffuse)
            .expect("entry checked above")
            .normal = Some(normal_handle);
    }

    /// Create the page's companion normal texture (zero-initialized) and
    /// rebuild the page bind group to bind it. No-op if it already exists.
    fn ensure_page_normal(&mut self, device: &wgpu::Device, page: usize) {
        if self.pages[page].normal.is_some() {
            return;
        }
        let size = self.pages[page].size;
        let normal = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("sprite page normal companion"),
            size: wgpu::Extent3d {
                width: size.x as u32,
                height: size.y as u32,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            // Linear: normal maps are data, never sRGB-decoded.
            format: NORMAL_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let normal_view = normal.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = self.page_bind_group(
            device,
            &self.pages[page].view,
            &normal_view,
            "sprite page (with normals)",
        );
        self.pages[page].bind_group = bind_group;
        self.pages[page].normal = Some(normal);
    }

    /// Build a page bind group from its diffuse + normal views.
    fn page_bind_group(
        &self,
        device: &wgpu::Device,
        view: &wgpu::TextureView,
        normal_view: &wgpu::TextureView,
        label: &str,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(label),
            layout: &self.texture_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(normal_view),
                },
            ],
        })
    }

    /// Find (or open) an atlas page with room for `w × h`; returns
    /// `(page index, x, y)`.
    fn atlas_spot(&mut self, device: &wgpu::Device, w: u32, h: u32) -> (usize, u32, u32) {
        for atlas in &mut self.atlases {
            if let Some((x, y)) = atlas.packer.insert(w, h) {
                return (atlas.page, x, y);
            }
        }
        let page = self.create_page(
            device,
            ATLAS_PAGE_SIZE,
            ATLAS_PAGE_SIZE,
            "sprite atlas page",
        );
        let mut packer = ShelfPacker::new(ATLAS_PAGE_SIZE, ATLAS_PAGE_SIZE);
        let (x, y) = packer
            .insert(w, h)
            .expect("fresh atlas page must fit a <=MAX_ATLAS_DIM sprite");
        self.atlases.push(AtlasPage { page, packer });
        (page, x, y)
    }

    /// Create an empty sRGB page texture + bind group; returns its index.
    fn create_page(&mut self, device: &wgpu::Device, w: u32, h: u32, label: &str) -> usize {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            // Raw (no sRGB decode): 2D blending runs in gamma space like
            // Godot (module docs / D-11).
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = self.page_bind_group(device, &view, &self.default_normal_view, label);
        self.pages.push(Page {
            texture,
            view,
            normal: None,
            bind_group,
            size: Vec2::new(w as f32, h as f32),
        });
        self.pages.len() - 1
    }

    /// Convert a **pre-sorted** [`DrawList`] into batched GPU instances and
    /// upload them (one buffer write) together with the camera uniforms.
    /// `target_size` is the render target's pixel size — per-instance
    /// `clip` rects are scaled by `clip_scale` (layout units → target
    /// pixels; `1.0` when the target IS the layout space), clamped to the
    /// target and become per-batch scissors; fully clipped-out instances
    /// are dropped here. World-channel instances are laid out in the world
    /// camera's `world_units` (their `pos`/`scale` are pixels by default);
    /// the screen channel is always y-down logical pixels. Call once per
    /// frame; [`draw_world`](Self::draw_world) and
    /// [`draw_screen`](Self::draw_screen) then encode the batches into
    /// their passes. Instances whose texture was never uploaded are skipped.
    #[allow(clippy::too_many_arguments)]
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        list: &DrawList,
        world_vp: Mat4,
        world_units: WorldUnits,
        screen_vp: Mat4,
        target_size: (u32, u32),
        clip_scale: f32,
    ) {
        // --- Convert + batch. Scratch vectors persist across frames.
        self.raw.clear();
        self.batches.clear();
        self.target_size = target_size;
        let channels = [
            (0usize, &list.world, world_units),
            (1usize, &list.screen, WorldUnits::default()),
        ];
        for (camera, channel, units) in channels {
            for inst in channel {
                let Some(entry) = self.entries.get(&inst.texture) else {
                    continue; // never uploaded — skip, never panic mid-frame
                };
                let scissor = match &inst.clip {
                    // `x * 1.0 == x` exactly: the game path (scale 1) is
                    // bit-identical to the pre-scale behavior (D-11).
                    Some(clip) => match scissor_px(
                        &Rect {
                            min: clip.min * clip_scale,
                            max: clip.max * clip_scale,
                        },
                        target_size,
                    ) {
                        Some(s) => Some(s),
                        None => continue, // clip covers nothing — cull
                    },
                    None => None,
                };
                let page = entry.page;
                let raw = to_raw(inst, entry, self.pages[page].size, units);
                let index = self.raw.len() as u32;
                self.raw.push(raw);
                match self.batches.last_mut() {
                    Some(b) if b.camera == camera && b.page == page && b.scissor == scissor => {
                        b.count += 1;
                    }
                    _ => self.batches.push(Batch {
                        camera,
                        page,
                        first: index,
                        count: 1,
                        scissor,
                    }),
                }
            }
        }
        if self.raw.is_empty() {
            return;
        }

        // --- Upload cameras + instances (grow the buffer as needed).
        queue.write_buffer(
            &self.cameras[0].buffer,
            0,
            bytemuck::cast_slice(&world_vp.to_cols_array()),
        );
        queue.write_buffer(
            &self.cameras[1].buffer,
            0,
            bytemuck::cast_slice(&screen_vp.to_cols_array()),
        );
        let needed = self.raw.len() as u64;
        if needed > self.instance_capacity {
            let new_cap = needed.next_power_of_two();
            self.instance_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("sprite instance buffer"),
                size: new_cap * size_of::<InstanceRaw>() as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.instance_capacity = new_cap;
        }
        queue.write_buffer(&self.instance_buffer, 0, bytemuck::cast_slice(&self.raw));
    }

    /// Instance-buffer capacity in instances; grows only past it.
    #[cfg(all(test, not(target_arch = "wasm32")))]
    pub(crate) fn instance_capacity(&self) -> u64 {
        self.instance_capacity
    }

    /// Texture pages (shared atlas + standalone) opened so far.
    #[cfg(all(test, not(target_arch = "wasm32")))]
    pub(crate) fn page_count(&self) -> usize {
        self.pages.len()
    }

    /// Encode the world-channel batches into the scene MRT pass.
    pub fn draw_world(&self, pass: &mut wgpu::RenderPass<'_>) {
        self.draw_channel(pass, &self.scene_pipeline, 0);
    }

    /// Encode the screen-channel batches into the (post-composite,
    /// single-target) UI pass.
    pub fn draw_screen(&self, pass: &mut wgpu::RenderPass<'_>) {
        self.draw_channel(pass, &self.screen_pipeline, 1);
    }

    fn draw_channel(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        pipeline: &wgpu::RenderPipeline,
        camera: usize,
    ) {
        if !self.batches.iter().any(|b| b.camera == camera) {
            return;
        }
        pass.set_pipeline(pipeline);
        pass.set_vertex_buffer(0, self.quad_vb.slice(..));
        pass.set_vertex_buffer(1, self.instance_buffer.slice(..));
        pass.set_bind_group(0, &self.cameras[camera].bind_group, &[]);
        let (tw, th) = self.target_size;
        let mut scissor_active = false;
        for batch in self.batches.iter().filter(|b| b.camera == camera) {
            match batch.scissor {
                Some([x, y, w, h]) => {
                    pass.set_scissor_rect(x, y, w, h);
                    scissor_active = true;
                }
                None if scissor_active => {
                    pass.set_scissor_rect(0, 0, tw, th);
                    scissor_active = false;
                }
                None => {}
            }
            pass.set_bind_group(1, &self.pages[batch.page].bind_group, &[]);
            pass.draw(
                0..QUAD_VERTS.len() as u32,
                batch.first..batch.first + batch.count,
            );
        }
        if scissor_active {
            pass.set_scissor_rect(0, 0, tw, th);
        }
    }
}

/// Write a decoded texture's pixels into `texture` at `(x, y)`.
fn write_pixels(queue: &wgpu::Queue, texture: &wgpu::Texture, x: u32, y: u32, tex: &Texture) {
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d { x, y, z: 0 },
            aspect: wgpu::TextureAspect::All,
        },
        &tex.rgba,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(4 * tex.width),
            rows_per_image: Some(tex.height),
        },
        wgpu::Extent3d {
            width: tex.width,
            height: tex.height,
            depth_or_array_layers: 1,
        },
    );
}

/// The 2×3 affine placing a sprite's unit quad in its camera's coordinates:
/// the `x`/`y` columns are the quad's texture-right and texture-down axes
/// (rotated, sized) and the translation is the unit quad's `(0,0)` corner —
/// the texture's top-left texel. `size_px` is the source rect size times
/// the instance scale, in pixels; the quad spans `size_px /
/// pixels_per_unit` in `units`, centered on `pos`.
///
/// `rot` is the standard rotation in world axes — on screen that reads
/// **clockwise** under the y-down pixel convention (Godot) and
/// **counter-clockwise** when `y_up`. When `y_up` the texture-down axis is
/// negated so the top row lands at the larger world y (the screen top)
/// instead of the sprite rendering upside-down. Flips are not applied here
/// (they fold into the UVs). Pure; unit-tested. The default units multiply
/// and divide by `1.0` only — exact, so the pixel path is bit-identical.
pub fn sprite_affine(pos: Vec2, rot: f32, size_px: Vec2, units: WorldUnits) -> Affine2 {
    let quad = size_px / units.pixels_per_unit;
    let (sin, cos) = rot.sin_cos();
    let col_x = Vec2::new(cos, sin) * quad.x;
    let col_y = Vec2::new(-sin, cos) * quad.y * units.screen_y_sign();
    // `pos` is the sprite center; the unit quad's origin is its corner.
    let translation = pos - (col_x + col_y) * 0.5;
    Affine2::from_cols(col_x, col_y, translation)
}

/// Convert one [`SpriteInstance`] to GPU form: build the centered 2×3 affine
/// ([`sprite_affine`] in `units`), resolve the src rect against the page
/// placement, fold flips into the UV endpoints, and pass the modulate color
/// through.
fn to_raw(inst: &SpriteInstance, entry: &Entry, page_size: Vec2, units: WorldUnits) -> InstanceRaw {
    let src = inst
        .src
        .unwrap_or(Rect::new(0.0, 0.0, entry.size.x, entry.size.y));
    let affine = sprite_affine(inst.pos, inst.rot, src.size() * inst.scale, units);

    // Page UVs; swapping an axis's endpoints flips it.
    let mut uv_min = (entry.offset + src.min) / page_size;
    let mut uv_max = (entry.offset + src.max) / page_size;
    if inst.flip_x {
        std::mem::swap(&mut uv_min.x, &mut uv_max.x);
    }
    if inst.flip_y {
        std::mem::swap(&mut uv_min.y, &mut uv_max.y);
    }

    // R-6: the instance samples the page's normal companion only when its
    // normal handle is the page's registered companion for this diffuse.
    let has_normal = inst.normal.is_some() && inst.normal == entry.normal;
    let bits = mask_bits(inst.light_mask);

    InstanceRaw {
        model_x: affine.matrix2.x_axis.to_array(),
        model_y: affine.matrix2.y_axis.to_array(),
        translation: affine.translation.to_array(),
        uv_min: uv_min.to_array(),
        uv_max: uv_max.to_array(),
        // Modulate passes through raw — Godot multiplies modulates on
        // sRGB values and the pipeline blends in gamma space (D-11).
        color: inst.color,
        misc: [
            bits[0],
            bits[1],
            bits[2],
            if has_normal { 1.0 } else { 0.0 },
        ],
        _pad: [0.0; 2],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    /// Logical clip rects clamp to the target and round outward; empty or
    /// fully off-target clips yield `None` (the instance is culled).
    #[wasm_bindgen_test(unsupported = test)]
    fn scissor_px_clamps_and_culls() {
        let target = (960, 540);
        assert_eq!(
            scissor_px(&Rect::new(10.5, 20.5, 100.0, 50.0), target),
            Some([10, 20, 101, 51])
        );
        // Clamped to the target edges.
        assert_eq!(
            scissor_px(&Rect::new(-50.0, -50.0, 2000.0, 2000.0), target),
            Some([0, 0, 960, 540])
        );
        // Fully off-target or empty → None.
        assert_eq!(
            scissor_px(&Rect::new(1000.0, 0.0, 50.0, 50.0), target),
            None
        );
        assert_eq!(
            scissor_px(&Rect::new(-100.0, 0.0, 50.0, 50.0), target),
            None
        );
        assert_eq!(scissor_px(&Rect::new(10.0, 10.0, 0.0, 0.0), target), None);
    }

    fn assert_close(a: Vec2, b: Vec2) {
        assert!((a - b).length() < 1e-4, "expected {b:?}, got {a:?}");
    }

    /// Pixel convention: a 32×16 px sprite centered at (100, 50) has its
    /// texture top-left at (84, 42) and spans +32 right, +16 down. A
    /// quarter-turn (positive) sends the texture-right axis **down** the
    /// screen — clockwise in y-down space (Godot).
    #[wasm_bindgen_test(unsupported = test)]
    fn sprite_affine_pixels_are_y_down_clockwise() {
        let units = WorldUnits::default();
        let a = sprite_affine(Vec2::new(100.0, 50.0), 0.0, Vec2::new(32.0, 16.0), units);
        assert_eq!(a.matrix2.x_axis, Vec2::new(32.0, 0.0));
        assert_eq!(a.matrix2.y_axis, Vec2::new(0.0, 16.0));
        assert_eq!(a.translation, Vec2::new(84.0, 42.0));
        assert_eq!(a.transform_point2(Vec2::ONE), Vec2::new(116.0, 58.0));

        let turned = sprite_affine(
            Vec2::ZERO,
            std::f32::consts::FRAC_PI_2,
            Vec2::new(32.0, 16.0),
            units,
        );
        assert_close(turned.matrix2.x_axis, Vec2::new(0.0, 32.0));
        assert_close(turned.matrix2.y_axis, Vec2::new(-16.0, 0.0));
    }

    /// At 32 px/unit, y-up, a 32×32 src at scale 1 spans exactly 1×1 units:
    /// the texture's top row sits at `pos.y + 0.5` (the larger world y) and
    /// its bottom row at `pos.y - 0.5`. A positive quarter-turn sends the
    /// texture-right axis to +y world — **up** the screen, counter-clockwise.
    #[wasm_bindgen_test(unsupported = test)]
    fn sprite_affine_units_y_up_spans_one_unit_top_up() {
        let units = WorldUnits {
            pixels_per_unit: 32.0,
            y_up: true,
        };
        let pos = Vec2::new(3.0, -2.0);
        let a = sprite_affine(pos, 0.0, Vec2::new(32.0, 32.0), units);
        assert_eq!(a.matrix2.x_axis, Vec2::new(1.0, 0.0));
        assert_eq!(a.matrix2.y_axis, Vec2::new(0.0, -1.0));
        // Corner (0,0) = texture top-left; (1,1) = texture bottom-right.
        assert_eq!(a.transform_point2(Vec2::ZERO), Vec2::new(2.5, -1.5));
        assert_eq!(a.transform_point2(Vec2::ONE), Vec2::new(3.5, -2.5));

        let turned = sprite_affine(pos, std::f32::consts::FRAC_PI_2, Vec2::splat(32.0), units);
        assert_close(turned.matrix2.x_axis, Vec2::new(0.0, 1.0));
        assert_close(turned.matrix2.y_axis, Vec2::new(1.0, 0.0));
    }

    /// Units without the flip scale only: at 32 px/unit y-down, a 64×32 src
    /// at scale (1, 2) is a 2×2 unit quad whose texture-down axis still
    /// points to +y, top-left at `pos - (1, 1)`.
    #[wasm_bindgen_test(unsupported = test)]
    fn sprite_affine_units_y_down_scales_only() {
        let units = WorldUnits {
            pixels_per_unit: 32.0,
            y_up: false,
        };
        let a = sprite_affine(
            Vec2::new(10.0, 20.0),
            0.0,
            Vec2::new(64.0, 32.0) * Vec2::new(1.0, 2.0),
            units,
        );
        assert_eq!(a.matrix2.x_axis, Vec2::new(2.0, 0.0));
        assert_eq!(a.matrix2.y_axis, Vec2::new(0.0, 2.0));
        assert_eq!(a.translation, Vec2::new(9.0, 19.0));
    }
}

/// Headless GPU tests: the real scene pipelines over a 4×4 target.
#[cfg(all(test, not(target_arch = "wasm32")))]
mod gpu_tests {
    use super::*;
    use crate::assets::Assets;
    use crate::canvas::camera::Camera;
    use crate::canvas::test_gpu::{device, read_texture, rgba16f_to_f32};

    const SIZE: u32 = 4;

    fn flat(width: u32, height: u32, rgba: [u8; 4]) -> Texture {
        Texture {
            width,
            height,
            rgba: rgba.repeat((width * height) as usize),
        }
    }

    /// Draw `list` through `pass` and return raw albedo and normal targets.
    fn draw_targets(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pass: &mut SpritePass,
        list: &DrawList,
        albedo_format: wgpu::TextureFormat,
        bytes_per_pixel: u32,
    ) -> (Vec<u8>, Vec<u8>) {
        let vp = Camera::new(SIZE, SIZE).view_proj();
        pass.prepare(
            device,
            queue,
            list,
            vp,
            WorldUnits::default(),
            vp,
            (SIZE, SIZE),
            1.0,
        );

        let target = |format: wgpu::TextureFormat, extra: wgpu::TextureUsages| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some("scene test target"),
                size: wgpu::Extent3d {
                    width: SIZE,
                    height: SIZE,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | extra,
                view_formats: &[],
            })
        };
        let albedo = target(albedo_format, wgpu::TextureUsages::COPY_SRC);
        let normal = target(NORMAL_FORMAT, wgpu::TextureUsages::COPY_SRC);
        let mask = target(MASK_FORMAT, wgpu::TextureUsages::empty());
        let views: Vec<_> = [&albedo, &normal, &mask]
            .into_iter()
            .map(|t| t.create_view(&wgpu::TextureViewDescriptor::default()))
            .collect();
        let attachments: Vec<_> = views
            .iter()
            .map(|view| {
                Some(wgpu::RenderPassColorAttachment {
                    view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })
            })
            .collect();
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        {
            let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("scene test pass"),
                color_attachments: &attachments,
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.draw_world(&mut rp);
        }
        queue.submit(Some(encoder.finish()));
        (
            read_texture(device, queue, &albedo, SIZE, SIZE, bytes_per_pixel),
            read_texture(device, queue, &normal, SIZE, SIZE, 4),
        )
    }

    fn draw_albedo(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pass: &mut SpritePass,
        list: &DrawList,
        albedo_format: wgpu::TextureFormat,
        bytes_per_pixel: u32,
    ) -> Vec<u8> {
        draw_targets(device, queue, pass, list, albedo_format, bytes_per_pixel).0
    }

    /// Draw one `texel`-colored 1×1 sprite scaled over the whole 4×4 target
    /// through `space`'s scene pipeline and read the albedo target back
    /// (raw bytes in that space's albedo format). `None` without an adapter.
    fn scene_albedo(space: LightingSpace, texel: [u8; 4]) -> Option<Vec<u8>> {
        let (device, queue) = device()?;
        let (albedo_format, bytes_per_pixel) = match space {
            LightingSpace::Gamma => (wgpu::TextureFormat::Rgba8Unorm, 4),
            LightingSpace::Linear => (wgpu::TextureFormat::Rgba16Float, 8),
        };
        let mut pass = SpritePass::new(
            &device,
            &queue,
            albedo_format,
            wgpu::TextureFormat::Rgba8Unorm,
            space,
        );
        let mut assets = Assets::<Texture>::new();
        let handle = assets.insert(
            "texel.png".into(),
            Texture {
                width: 1,
                height: 1,
                rgba: texel.to_vec(),
            },
        );
        pass.upload(
            &device,
            &queue,
            handle,
            assets.get(handle).expect("inserted"),
        );

        let mut list = DrawList::new();
        list.push(SpriteInstance {
            scale: Vec2::splat(SIZE as f32),
            ..SpriteInstance::new(handle, Vec2::splat(SIZE as f32 / 2.0))
        });
        Some(draw_albedo(
            &device,
            &queue,
            &mut pass,
            &list,
            albedo_format,
            bytes_per_pixel,
        ))
    }

    /// Replacing texture pixels preserves the draw handle. Equal dimensions
    /// update the existing placement and keep its compatible normal map;
    /// changed dimensions relocate the diffuse and disable the stale normal.
    #[test]
    fn replacement_updates_pixels_and_detaches_incompatible_normal() {
        let Some((device, queue)) = device() else {
            return;
        };
        let mut pass = SpritePass::new(
            &device,
            &queue,
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureFormat::Rgba8Unorm,
            LightingSpace::Gamma,
        );
        let mut assets = Assets::<Texture>::new();
        let diffuse = assets.insert("diffuse.png".into(), flat(1, 1, [220, 30, 20, 255]));
        let normal = assets.insert("normal.png".into(), flat(1, 1, [128, 128, 255, 255]));
        pass.upload(&device, &queue, diffuse, assets.get(diffuse).unwrap());
        pass.upload_normal(
            &device,
            &queue,
            diffuse,
            normal,
            assets.get(normal).unwrap(),
        );

        let mut list = DrawList::new();
        list.push(SpriteInstance {
            scale: Vec2::splat(SIZE as f32),
            normal: Some(normal),
            ..SpriteInstance::new(diffuse, Vec2::splat(SIZE as f32 / 2.0))
        });

        assert!(pass.replace(&device, &queue, diffuse, &flat(1, 1, [20, 210, 40, 255]),));
        let pixels = draw_albedo(
            &device,
            &queue,
            &mut pass,
            &list,
            wgpu::TextureFormat::Rgba8Unorm,
            4,
        );
        assert!(
            pixels
                .chunks_exact(4)
                .all(|pixel| pixel == [20, 210, 40, 255]),
            "same-size replacement did not reach the GPU: {:?}",
            &pixels[..4]
        );
        assert_eq!(pass.raw[0].misc[3], 1.0, "compatible normal was detached");

        let resized = Texture {
            width: 2,
            height: 1,
            rgba: [30, 50, 230, 255, 230, 180, 20, 255].to_vec(),
        };
        assert!(pass.replace(&device, &queue, diffuse, &resized));
        let mut resized_list = DrawList::new();
        resized_list.push(SpriteInstance {
            scale: Vec2::new(2.0, SIZE as f32),
            normal: Some(normal),
            ..SpriteInstance::new(diffuse, Vec2::splat(SIZE as f32 / 2.0))
        });
        let pixels = draw_albedo(
            &device,
            &queue,
            &mut pass,
            &resized_list,
            wgpu::TextureFormat::Rgba8Unorm,
            4,
        );
        for row in pixels.chunks_exact((SIZE * 4) as usize) {
            assert_eq!(&row[..8], [30, 50, 230, 255, 30, 50, 230, 255]);
            assert_eq!(&row[8..], [230, 180, 20, 255, 230, 180, 20, 255]);
        }
        assert_eq!(
            pass.raw[0].misc[3], 0.0,
            "resized diffuse kept an incompatible normal association"
        );

        let never_uploaded = assets.insert("unused.png".into(), flat(1, 1, [0, 0, 0, 255]));
        assert!(!pass.replace(
            &device,
            &queue,
            never_uploaded,
            assets.get(never_uploaded).unwrap(),
        ));
    }

    /// A normal-map handle is replaceable even when it has no diffuse entry.
    /// Every diffuse that shares it observes the new pixels. If the same
    /// handle later gains a diffuse role, one replacement updates both roles;
    /// a size mismatch detaches every incompatible owner.
    #[test]
    fn normal_replacement_updates_every_owner_and_an_optional_diffuse_role() {
        let Some((device, queue)) = device() else {
            return;
        };
        let mut pass = SpritePass::new(
            &device,
            &queue,
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureFormat::Rgba8Unorm,
            LightingSpace::Gamma,
        );
        let mut assets = Assets::<Texture>::new();
        let left = assets.insert("left.png".into(), flat(1, 1, [255; 4]));
        let right = assets.insert("right.png".into(), flat(1, 1, [255; 4]));
        let normal = assets.insert("shared-normal.png".into(), flat(1, 1, [255, 128, 128, 255]));
        for diffuse in [left, right] {
            pass.upload(&device, &queue, diffuse, assets.get(diffuse).unwrap());
            pass.upload_normal(
                &device,
                &queue,
                diffuse,
                normal,
                assets.get(normal).unwrap(),
            );
        }
        assert!(
            !pass.has_texture(normal),
            "normal unexpectedly had a diffuse role"
        );

        let mut owners = DrawList::new();
        for (diffuse, x) in [(left, 1.0), (right, 3.0)] {
            owners.push(SpriteInstance {
                scale: Vec2::new(2.0, SIZE as f32),
                normal: Some(normal),
                ..SpriteInstance::new(diffuse, Vec2::new(x, 2.0))
            });
        }
        let new_normal = flat(1, 1, [128, 255, 128, 255]);
        assert!(pass.replace(&device, &queue, normal, &new_normal));
        let (_, normal_pixels) = draw_targets(
            &device,
            &queue,
            &mut pass,
            &owners,
            wgpu::TextureFormat::Rgba8Unorm,
            4,
        );
        assert!(
            normal_pixels
                .chunks_exact(4)
                .all(|pixel| pixel == [128, 0, 128, 255]),
            "shared companion pixels were not replaced for both owners"
        );

        pass.upload(&device, &queue, normal, &flat(1, 1, [10, 20, 30, 255]));
        let both_roles = flat(1, 1, [240, 30, 60, 255]);
        assert!(pass.replace(&device, &queue, normal, &both_roles));
        let mut diffuse_draw = DrawList::new();
        diffuse_draw.push(SpriteInstance {
            scale: Vec2::splat(SIZE as f32),
            ..SpriteInstance::new(normal, Vec2::splat(2.0))
        });
        let diffuse_pixels = draw_albedo(
            &device,
            &queue,
            &mut pass,
            &diffuse_draw,
            wgpu::TextureFormat::Rgba8Unorm,
            4,
        );
        assert!(
            diffuse_pixels
                .chunks_exact(4)
                .all(|pixel| pixel == [240, 30, 60, 255]),
            "the handle's diffuse role was not replaced"
        );
        let (_, normal_pixels) = draw_targets(
            &device,
            &queue,
            &mut pass,
            &owners,
            wgpu::TextureFormat::Rgba8Unorm,
            4,
        );
        assert!(
            normal_pixels
                .chunks_exact(4)
                .all(|pixel| pixel == [240, 225, 60, 255]),
            "the handle's companion roles were not replaced"
        );

        assert!(pass.replace(&device, &queue, normal, &flat(2, 1, [70, 80, 90, 255]),));
        let (_, normal_pixels) = draw_targets(
            &device,
            &queue,
            &mut pass,
            &owners,
            wgpu::TextureFormat::Rgba8Unorm,
            4,
        );
        assert!(
            normal_pixels
                .chunks_exact(4)
                .all(|pixel| pixel == [128, 128, 255, 255]),
            "mismatched shared normal remained associated"
        );
    }

    /// Crossing the atlas threshold in either direction produces a valid new
    /// placement under the original handle and renders the replacement data.
    #[test]
    fn replacement_crosses_between_atlas_and_standalone() {
        let Some((device, queue)) = device() else {
            return;
        };
        let mut pass = SpritePass::new(
            &device,
            &queue,
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureFormat::Rgba8Unorm,
            LightingSpace::Gamma,
        );
        let mut assets = Assets::<Texture>::new();
        let handle = assets.insert("threshold.png".into(), flat(1, 1, [10, 20, 30, 255]));
        pass.upload(&device, &queue, handle, assets.get(handle).unwrap());
        assert_eq!(pass.page_count(), 1);

        let standalone = flat(MAX_ATLAS_DIM + 1, 1, [25, 75, 225, 255]);
        assert!(pass.replace(&device, &queue, handle, &standalone));
        assert_eq!(pass.page_count(), 2, "standalone page was not allocated");
        let mut list = DrawList::new();
        list.push(SpriteInstance {
            src: Some(Rect::new(0.0, 0.0, 1.0, 1.0)),
            scale: Vec2::splat(SIZE as f32),
            ..SpriteInstance::new(handle, Vec2::splat(2.0))
        });
        let pixels = draw_albedo(
            &device,
            &queue,
            &mut pass,
            &list,
            wgpu::TextureFormat::Rgba8Unorm,
            4,
        );
        assert!(pixels.chunks_exact(4).all(|p| p == [25, 75, 225, 255]));

        assert!(pass.replace(&device, &queue, handle, &flat(1, 1, [210, 45, 15, 255]),));
        assert_eq!(
            pass.page_count(),
            2,
            "returning to the atlas unnecessarily opened another page"
        );
        let pixels = draw_albedo(
            &device,
            &queue,
            &mut pass,
            &list,
            wgpu::TextureFormat::Rgba8Unorm,
            4,
        );
        assert!(pixels.chunks_exact(4).all(|p| p == [210, 45, 15, 255]));
    }

    /// The linear scene pipeline decodes the page texel: an sRGB 128
    /// (0.50196) lands in the half-float albedo as
    /// `((0.50196 + 0.055) / 1.055)^2.4 ≈ 0.2159`, opaque, on every pixel.
    #[test]
    fn linear_scene_pass_decodes_srgb_texels() {
        let Some(bytes) = scene_albedo(LightingSpace::Linear, [128, 128, 128, 255]) else {
            return;
        };
        let px = rgba16f_to_f32(&bytes);
        for p in px.chunks_exact(4) {
            for c in &p[..3] {
                assert!((c - 0.2159).abs() < 2e-3, "{p:?}");
            }
            assert!((p[3] - 1.0).abs() < 1e-3, "{p:?}");
        }
    }

    /// The gamma scene pipeline stores the texel raw: sRGB 128 stays 128.
    #[test]
    fn gamma_scene_pass_stores_texels_raw() {
        let Some(bytes) = scene_albedo(LightingSpace::Gamma, [128, 128, 128, 255]) else {
            return;
        };
        for p in bytes.chunks_exact(4) {
            assert_eq!(p, [128, 128, 128, 255], "{p:?}");
        }
    }
}
