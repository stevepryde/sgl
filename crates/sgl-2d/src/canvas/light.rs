//! 2D lighting (R-6, PR-4/AR-6): the [`LightFrame`] seam + the GPU light
//! passes, replicating Godot's additive 2D light model.
//!
//! **Model** (PR-4, Godot "add" mode): the scene renders full-bright into an
//! albedo target; each point light *adds* `cookie × color × energy ×
//! normal_factor × shadow × coverage` on top; the composite is
//! `albedo × (canvas_modulate + Σ lights)` — i.e. the base scene modulated by
//! the canvas color plus every light's contribution multiplied by the base
//! albedo (exactly Godot's `base·modulate + Σ light·base`). With the default
//! white modulate and no lights this is the identity, so unlit scenes (menus,
//! demos) render unchanged.
//!
//! **Color space** (R-13 parity, D-11): Godot blends 2D in sRGB ("gamma")
//! space, so by default the light math here stays in gamma space too —
//! cookies are sampled raw (non-sRGB texture format), light colors and
//! `energy` upload as given (sRGB), and the accumulation target holds
//! gamma-space contributions the composite multiplies straight into the
//! gamma albedo. Under `LightingSpace::Linear` (`Renderer::with_lighting`)
//! this pass is *unchanged*: colors and `energy` still upload as given —
//! now meaning linear — and the composite multiplies the accumulation into a
//! linear albedo before encoding once. Cookies stay raw data in both modes.
//!
//! **Mechanisms**:
//! - **Cookies**: a light with `cookie: Some` is a texture
//!   (`assets/lights/*.png`) whose world footprint is `texture_size ×
//!   texture_scale`, centered on the light (lights live in world space and
//!   move with the camera). The cookie's intensity is `rgb × alpha` (the
//!   shipped cookies carry the falloff in alpha over a constant color).
//! - **Analytic lights**: a light with `cookie: None` has a `±radius`
//!   footprint and the `bevy_light_2d` radial intensity
//!   ([`analytic_attenuation`]: `(1 − s²)² / (1 + falloff·s²)`,
//!   `s = dist / radius`). Both kinds run through the one light pipeline:
//!   the shader computes both intensities and selects by the light's
//!   `radius` (0 = cookie), with a shared 1×1 white dummy cookie bound for
//!   analytic lights so the cookie bind group is always valid.
//! - **Normal mapping**: the scene pass writes a screen-space normal target
//!   (see `sprite.wgsl`); the light pass computes
//!   `max(dot(N, normalize(light_pos - frag, height)), 0)` and blends it to
//!   `1` where no normal map was written (Godot: sprites without a normal map
//!   are lit at full strength).
//! - **Light layers**: sprites carry a `light_mask` (layers 1/2/4 → the mask
//!   target's r/g/b); each light carries an `item_mask`; the light only
//!   contributes where the masks intersect ([`coverage`]). Alpha-blended
//!   sprites leave *fractional* coverage — a soft per-pixel approximation of
//!   Godot's per-item masking.
//! - **Shadows**: CPU-extruded shadow geometry (AR-6, Godot-style). For each
//!   occluder polygon edge, a quad (capped by a far vertex on the bisector
//!   when the edge spans more than 90° from the light) is extruded away from
//!   the light past its footprint ([`shadow_triangles`]); the union over
//!   all edges darkens everything *behind* the first surface seen from the
//!   light — matching Godot's 1D shadow map occlusion shape. Per shadow-casting light the triangles render
//!   into a shared R8 mask target (white = lit, black = shadow), which the
//!   light pass samples with a 3×3 box tap for a soft, PCF-ish edge
//!   (approximating Godot's `filter smooth 5.0`).
//!
//! **Budget**: up to [`MAX_LIGHTS`] lights per frame after view culling;
//! shadows on the first [`MAX_SHADOW_LIGHTS`] shadow-casting lights (extras
//! render unshadowed).

use std::collections::HashMap;

use glam::{Mat4, Vec2, Vec3, Vec4};
use wgpu::util::DeviceExt;

use crate::assets::{Handle, Texture};
use crate::canvas::camera::WorldUnits;
use crate::canvas::sprite::TextureError;

/// Max active lights per frame (after view culling); extras are dropped.
pub const MAX_LIGHTS: usize = 32;

/// Max shadow-casting lights per frame; extra shadowed lights render without
/// shadows (PR-4 keeps in-game shadow lights well under this).
pub const MAX_SHADOW_LIGHTS: usize = 8;

/// Minimum shadow extrusion length in world pixels — longer than any cookie
/// footprint (the largest cookie is 512 px at `texture_scale` 3.0 in the
/// menu). Divided by the camera's `pixels_per_unit` so it is the same screen
/// length in a world-unit camera. Lights with a larger footprint (a big
/// analytic `radius`) extrude to twice their footprint's half-diagonal
/// instead, so shadows always cover the footprint ([`shadow_triangles`]).
const SHADOW_FAR: f32 = 4096.0;

/// The light-accumulation target format: float so overlapping lights can sum
/// past 1.0 before the composite clamps (Godot's additive behavior).
pub const ACCUM_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// The per-light shadow mask format (1 = lit, 0 = shadowed).
pub const SHADOW_MASK_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R8Unorm;

/// One additive point light (the Godot `Light2D` in "add" mode, PR-4).
///
/// Two footprint/intensity models share the one light pipeline:
/// - **cookie** (`cookie: Some`): footprint = cookie pixel size ×
///   `texture_scale`; intensity = the sampled `cookie.rgb × cookie.a`;
/// - **analytic** (`cookie: None`): footprint = `±radius` world pixels;
///   intensity = [`analytic_attenuation`] of the distance to `pos`.
///
/// Either way the contribution is `intensity × color × energy ×
/// normal_factor × coverage × shadow`.
#[derive(Debug, Clone, Copy)]
pub struct PointLight {
    /// Light position in the world camera's units (logical pixels by
    /// default; world units under `Camera::with_units`).
    pub pos: Vec2,
    /// The cookie texture (must be registered via
    /// `Renderer::upload_light_cookie`; unknown cookies skip the light), or
    /// `None` for an analytic light shaped by `radius` / `falloff`.
    pub cookie: Option<Handle<Texture>>,
    /// Analytic lights only: the footprint half-extent in the world camera's
    /// units (pixels by default; no `pixels_per_unit` scaling applies) — the
    /// light reaches exactly `radius` from `pos`. Must be finite and `> 0`;
    /// any other value skips the light. Ignored by cookie lights.
    pub radius: f32,
    /// Analytic lights only: the falloff steepness of
    /// [`analytic_attenuation`] (`0` = the bare `(1 − s²)²` bell; shadow-sp's
    /// player glow uses 72). Must be finite and `≥ 0`; any other value skips
    /// the light. Ignored by cookie lights.
    pub falloff: f32,
    /// Light color, used as given: sRGB like Godot's `color` property under
    /// the default gamma lighting, linear under `LightingSpace::Linear`.
    pub color: [f32; 3],
    /// Godot `energy`: linear multiplier on the contribution (may exceed 1).
    pub energy: f32,
    /// Godot `texture_scale`: scales the cookie's world footprint
    /// (`cookie_px × texture_scale` pixels = that over `pixels_per_unit`
    /// units; see [`cookie_half_extent`]). Ignored by analytic lights.
    pub texture_scale: f32,
    /// Godot `height`: the light's z for normal-map shading, in the world
    /// camera's units (Godot's 128 px is 4.0 in a 32 px/unit camera).
    pub height: f32,
    /// Which sprite light-mask layers this light affects (bits 1|2|4 —
    /// Godot `range_item_cull_mask`).
    pub item_mask: u32,
    /// Whether occluder polygons cast shadows from this light.
    pub shadows: bool,
}

impl PointLight {
    /// A white light of `cookie` at `pos` with Godot-ish defaults: energy 1,
    /// `texture_scale` 1, height 128, item mask layer 1, no shadows.
    pub fn new(cookie: Handle<Texture>, pos: Vec2) -> Self {
        Self {
            pos,
            cookie: Some(cookie),
            radius: 0.0,
            falloff: 0.0,
            color: [1.0, 1.0, 1.0],
            energy: 1.0,
            texture_scale: 1.0,
            height: 128.0,
            item_mask: 1,
            shadows: false,
        }
    }

    /// A white analytic (cookie-less) light at `pos` reaching `radius` world
    /// pixels with the given `falloff` (see [`analytic_attenuation`]); the
    /// other fields take the same defaults as [`new`](Self::new).
    pub fn analytic(pos: Vec2, radius: f32, falloff: f32) -> Self {
        Self {
            pos,
            cookie: None,
            radius,
            falloff,
            color: [1.0, 1.0, 1.0],
            energy: 1.0,
            texture_scale: 1.0,
            height: 128.0,
            item_mask: 1,
            shadows: false,
        }
    }
}

/// A frame's worth of lighting — the game→renderer seam next to `DrawList`.
///
/// The default (white modulate, no lights, no occluders) is the identity:
/// `Renderer::render` uses it so unlit callers are untouched.
#[derive(Debug, Clone)]
pub struct LightFrame {
    /// Godot `CanvasModulate`: multiplies the base scene before lights add.
    /// Used as given — sRGB under the default gamma lighting (the in-game
    /// scene has none = white, the menu uses 0.0824 gray), linear under
    /// `LightingSpace::Linear` (shadow-sp's linear ambient × brightness).
    pub canvas_modulate: [f32; 4],
    /// The frame's point lights, world space.
    pub lights: Vec<PointLight>,
    /// Shadow-occluder polygons in world space (PR-3 wall occluders). Static
    /// per level — callers may fill this once and reuse the frame.
    pub occluders: Vec<Vec<Vec2>>,
}

impl Default for LightFrame {
    fn default() -> Self {
        Self {
            canvas_modulate: [1.0, 1.0, 1.0, 1.0],
            lights: Vec::new(),
            occluders: Vec::new(),
        }
    }
}

// --- Pure light math (the shaders' reference implementations) ---------------

/// Whether an analytic light's shape parameters are usable: a finite,
/// positive `radius` and a finite, non-negative `falloff` (a negative
/// falloff would put a pole of the attenuation inside the footprint).
/// [`LightPass::prepare`] skips lights that fail this instead of panicking
/// or uploading NaNs.
#[must_use]
pub fn analytic_params_valid(radius: f32, falloff: f32) -> bool {
    radius.is_finite() && radius > 0.0 && falloff.is_finite() && falloff >= 0.0
}

/// The analytic point-light intensity `dist` world pixels from the light
/// (`bevy_light_2d` semantics — shadow-sp's `REFERENCE_PORT` §6.4): with
/// `s = dist / radius`, `(1 − s²)² / (1 + falloff·s²)` for `s ≤ 1` and `0`
/// beyond the radius. Invalid parameters ([`analytic_params_valid`]) give
/// `0`. Mirrors `fs_light` in `light.wgsl`, which evaluates the same
/// expression on the light quad's normalized offset.
#[must_use]
pub fn analytic_attenuation(dist: f32, radius: f32, falloff: f32) -> f32 {
    if !analytic_params_valid(radius, falloff) {
        return 0.0;
    }
    let s = dist / radius;
    let s2 = s * s;
    let bell = (1.0 - s2).max(0.0);
    bell * bell / (1.0 + falloff * s2)
}

/// The PR-4 normal-shading factor: `max(dot(N, L), 0)` with
/// `L = normalize(light_pos - frag_pos, height)`. `normal` is the decoded
/// screen-space normal (y-down), so `light_pos`/`frag_pos` are y-down too
/// (under a y-up camera the shader negates the y delta first); a degenerate
/// normal (length ≈ 0, the "no normal map" encoding) returns 1 — Godot
/// lights unmapped sprites at full strength. Mirrors `fs_light` in
/// `light.wgsl`.
#[must_use]
pub fn normal_factor(light_pos: Vec2, height: f32, frag_pos: Vec2, normal: [f32; 3]) -> f32 {
    let n = Vec3::from_array(normal);
    let n_len = n.length();
    if n_len < 1e-3 {
        return 1.0;
    }
    let l = Vec3::new(light_pos.x - frag_pos.x, light_pos.y - frag_pos.y, height);
    let l_len = l.length();
    if l_len < 1e-6 {
        return 1.0;
    }
    ((n / n_len).dot(l / l_len)).max(0.0)
}

/// A cookie's half footprint in the camera's units: `cookie_px ×
/// texture_scale / 2` pixels over `pixels_per_unit` (1 under the pixel
/// convention, where `x / 1.0 == x` exactly).
#[must_use]
pub fn cookie_half_extent(cookie_px: Vec2, texture_scale: f32, pixels_per_unit: f32) -> Vec2 {
    cookie_px * texture_scale * 0.5 / pixels_per_unit
}

/// Split a light-layer mask into the mask target's r/g/b channels
/// (layers 1, 2, 4 — the only layers the game uses, PR-4). Higher bits are
/// ignored.
#[must_use]
pub fn mask_bits(mask: u32) -> [f32; 3] {
    [
        if mask & 1 != 0 { 1.0 } else { 0.0 },
        if mask & 2 != 0 { 1.0 } else { 0.0 },
        if mask & 4 != 0 { 1.0 } else { 0.0 },
    ]
}

/// Whether a sprite `light_mask` intersects a light `item_mask` on the three
/// supported layers.
#[must_use]
pub fn mask_matches(light_mask: u32, item_mask: u32) -> bool {
    light_mask & item_mask & 0b111 != 0
}

/// Per-pixel receiver coverage: the mask target's rgb (fractional layer
/// coverage) dotted with the light's layer selectors, clamped to 1. Mirrors
/// `fs_light`.
#[must_use]
pub fn coverage(mask_rgb: [f32; 3], item_mask: u32) -> f32 {
    let bits = mask_bits(item_mask);
    (mask_rgb[0] * bits[0] + mask_rgb[1] * bits[1] + mask_rgb[2] * bits[2]).clamp(0.0, 1.0)
}

/// Extrude shadow geometry for one light: for **every** edge `(a, b)` of every
/// occluder polygon near the light, append the quad
/// `[a, b, b + dir(b)·far, a + dir(a)·far]` (as two triangles, 6 vertices)
/// where `dir(v) = normalize(v - light_pos)`.
///
/// When the edge subtends more than 90° at the light (a long wall close to
/// it), the straight far edge of that quad would pass close to the light and
/// leave the shadow behind the wall lit. Such an edge gets a third far
/// vertex on the bisector of `dir(a)` and `dir(b)` (a convex pentagon, 9
/// vertices), so every far edge stays at least `far·cos 45°` from the light.
/// Pass `far ≥ √2 ×` the footprint's half-diagonal (`|half_extent|`) and the
/// shadow covers everything behind the edge inside the footprint.
///
/// The union over all edges is exactly "everything behind the first surface
/// seen from the light" (Godot's occlusion shape): front-edge quads cover the
/// polygon interior and beyond; back-edge quads are subsets of that union —
/// so the result is winding-independent. Polygons whose AABB misses the
/// light's footprint AABB (`pos ± half_extent`) are culled.
pub fn shadow_triangles(
    light_pos: Vec2,
    half_extent: Vec2,
    occluders: &[Vec<Vec2>],
    far: f32,
    out: &mut Vec<[f32; 2]>,
) {
    let range_min = light_pos - half_extent;
    let range_max = light_pos + half_extent;
    for poly in occluders {
        if poly.len() < 2 {
            continue;
        }
        // AABB cull against the light footprint.
        let mut min = poly[0];
        let mut max = poly[0];
        for p in &poly[1..] {
            min = min.min(*p);
            max = max.max(*p);
        }
        if min.x > range_max.x || max.x < range_min.x || min.y > range_max.y || max.y < range_min.y
        {
            continue;
        }
        let extrude = |v: Vec2| -> Option<Vec2> {
            let d = v - light_pos;
            let len = d.length();
            if len < 1e-4 {
                return None; // Vertex on the light: degenerate, skip the edge.
            }
            Some(v + d / len * far)
        };
        let mut j = poly.len() - 1;
        for i in 0..poly.len() {
            let (a, b) = (poly[j], poly[i]);
            j = i;
            let (Some(ea), Some(eb)) = (extrude(a), extrude(b)) else {
                continue;
            };
            let (da, db) = ((ea - a).normalize(), (eb - b).normalize());
            let bisector = da + db;
            // Past 90°, cap the far side with a vertex on the bisector, as far
            // from the light as the farther extruded end: it lies beyond the
            // straight far edge, so the pentagon stays convex. A light on
            // the edge itself (opposite directions) has no area behind it.
            if da.dot(db) < 0.0 && bisector.length_squared() > 1e-8 {
                let reach = (ea - light_pos).length().max((eb - light_pos).length());
                let em = light_pos + bisector.normalize() * reach;
                out.extend_from_slice(&[
                    a.to_array(),
                    b.to_array(),
                    eb.to_array(),
                    a.to_array(),
                    eb.to_array(),
                    em.to_array(),
                    a.to_array(),
                    em.to_array(),
                    ea.to_array(),
                ]);
                continue;
            }
            out.extend_from_slice(&[
                a.to_array(),
                b.to_array(),
                eb.to_array(),
                a.to_array(),
                eb.to_array(),
                ea.to_array(),
            ]);
        }
    }
}

// --- GPU light pass ----------------------------------------------------------

/// Per-light GPU record (matches `Light` in `light.wgsl`).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct LightRaw {
    /// Light position xy + half footprint zw, in the camera's units.
    pos_half: [f32; 4],
    /// Light color rgb (as authored, see the module docs), energy in w.
    color_energy: [f32; 4],
    /// x = height, y = shadow flag (0/1), z = analytic radius (0 = cookie
    /// light), w = analytic falloff.
    params: [f32; 4],
    /// rgb = item-mask layer selectors, w pad.
    mask: [f32; 4],
}

impl LightRaw {
    const ZERO: Self = Self {
        pos_half: [0.0; 4],
        color_energy: [0.0; 4],
        params: [0.0; 4],
        mask: [0.0; 4],
    };
}

/// The light-pass uniform (matches `Uniforms` in `light.wgsl`).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct LightsUniform {
    view_proj: [[f32; 4]; 4],
    /// Logical target size (w, h); z = the world → screen-down y sign
    /// (`+1` y-down pixels, `-1` a y-up camera); w pad.
    screen: [f32; 4],
    lights: [LightRaw; MAX_LIGHTS],
}

/// One uploaded cookie texture: its bind group + pixel size.
struct Cookie {
    bind_group: wgpu::BindGroup,
    size: Vec2,
}

/// One planned light draw for this frame.
struct LightDraw {
    /// Index into the uniform's light array (= the draw's first instance).
    index: u32,
    /// The cookie to bind; `None` binds the shared white dummy (analytic).
    cookie: Option<Handle<Texture>>,
    /// Shadow-geometry vertex range in the shadow VB, when shadowed.
    shadow: Option<std::ops::Range<u32>>,
}

/// Owns the light-accumulation target, the shared shadow-mask target, the
/// per-light additive pipeline and the shadow-extrusion pipeline.
pub struct LightPass {
    /// The accumulation texture (`COPY_SRC` so it can be read back).
    accum_texture: wgpu::Texture,
    accum_view: wgpu::TextureView,
    shadow_mask_view: wgpu::TextureView,

    light_pipeline: wgpu::RenderPipeline,
    shadow_pipeline: wgpu::RenderPipeline,
    uniform: wgpu::Buffer,
    /// Layout of `frame_bind_group` (kept for [`LightPass::resize`]).
    frame_bgl: wgpu::BindGroupLayout,
    /// Uniform + normal/mask/shadow-mask textures (light pass, group 0).
    frame_bind_group: wgpu::BindGroup,
    /// Uniform only (shadow-mask pass — it renders *into* the shadow mask,
    /// so that texture cannot be bound there).
    shadow_bind_group: wgpu::BindGroup,

    cookie_bgl: wgpu::BindGroupLayout,
    cookie_sampler: wgpu::Sampler,
    cookies: HashMap<Handle<Texture>, Cookie>,
    /// A 1×1 opaque-white cookie bound for analytic lights, so group 1 is
    /// always valid and the pipeline never branches on light kind.
    analytic_bind_group: wgpu::BindGroup,

    shadow_vb: wgpu::Buffer,
    shadow_capacity: u64,

    // Per-frame scratch (allocations persist).
    verts: Vec<[f32; 2]>,
    plan: Vec<LightDraw>,
}

const INITIAL_SHADOW_VERTS: u64 = 1024;

/// Create the light pass's accumulation texture and shadow-mask view at the
/// given pixel size.
fn light_targets(
    device: &wgpu::Device,
    width: u32,
    height: u32,
) -> (wgpu::Texture, wgpu::TextureView) {
    let target = |label: &str, format: wgpu::TextureFormat, extra: wgpu::TextureUsages| {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | extra,
            view_formats: &[],
        })
    };
    (
        // Readable (tests / debug capture) like the composited target.
        target(
            "light accumulation target",
            ACCUM_FORMAT,
            wgpu::TextureUsages::COPY_SRC,
        ),
        target(
            "shadow mask target",
            SHADOW_MASK_FORMAT,
            wgpu::TextureUsages::empty(),
        )
        .create_view(&wgpu::TextureViewDescriptor::default()),
    )
}

/// A group-1 (cookie texture + sampler) bind group for the light pipeline.
fn cookie_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    sampler: &wgpu::Sampler,
    view: &wgpu::TextureView,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("light cookie bind group"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
        ],
    })
}

/// The light pass's group-0 bind group (uniform + normal/mask/shadow-mask).
fn frame_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    uniform: &wgpu::Buffer,
    normal_view: &wgpu::TextureView,
    mask_view: &wgpu::TextureView,
    shadow_mask_view: &wgpu::TextureView,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("light frame bind group"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(normal_view),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(mask_view),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::TextureView(shadow_mask_view),
            },
        ],
    })
}

impl LightPass {
    /// Build the light pipelines and targets. `normal_view`/`mask_view` are
    /// the scene pass's MRT outputs at the same target size (game scenes
    /// keep the fixed logical resolution, PR-1; the editor resizes all
    /// offscreen targets together through [`LightPass::resize`]). `queue`
    /// seeds the analytic lights' shared 1×1 white dummy cookie.
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        normal_view: &wgpu::TextureView,
        mask_view: &wgpu::TextureView,
        logical_width: u32,
        logical_height: u32,
    ) -> Self {
        let (accum_texture, shadow_mask_view) =
            light_targets(device, logical_width, logical_height);
        let accum_view = accum_texture.create_view(&wgpu::TextureViewDescriptor::default());

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("light shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("light.wgsl").into()),
        });

        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("lights uniform"),
            size: size_of::<LightsUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let uniform_entry = wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let tex_entry = |binding: u32| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };

        // Group 0 (light pass): uniform + the three screen-space inputs.
        let frame_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("light frame bgl"),
            entries: &[uniform_entry, tex_entry(1), tex_entry(2), tex_entry(3)],
        });
        let frame_bind_group = frame_bind_group(
            device,
            &frame_bgl,
            &uniform,
            normal_view,
            mask_view,
            &shadow_mask_view,
        );

        // Group 0 (shadow pass): the uniform alone — the shadow mask is this
        // pass's render target and must not be bound.
        let shadow_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("shadow bgl"),
            entries: &[uniform_entry],
        });
        let shadow_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("shadow bind group"),
            layout: &shadow_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform.as_entire_binding(),
            }],
        });

        // Group 1 (light pass): the cookie. Linear filtering — cookies are
        // smooth gradients drawn scaled (texture_scale), nearest would band.
        let cookie_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("light cookie bgl"),
            entries: &[
                tex_entry(0),
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let cookie_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("light cookie sampler (linear)"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });
        // Analytic lights bind this opaque-white 1×1 cookie: sampled it is
        // `rgb × a = 1`, and the shader selects the analytic intensity anyway.
        let white = device.create_texture_with_data(
            queue,
            &wgpu::TextureDescriptor {
                label: Some("analytic light dummy cookie (1x1 white)"),
                size: wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            },
            wgpu::util::TextureDataOrder::LayerMajor,
            &[255, 255, 255, 255],
        );
        let analytic_bind_group = cookie_bind_group(
            device,
            &cookie_bgl,
            &cookie_sampler,
            &white.create_view(&wgpu::TextureViewDescriptor::default()),
        );

        let light_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("light pipeline layout"),
            bind_group_layouts: &[Some(&frame_bgl), Some(&cookie_bgl)],
            immediate_size: 0,
        });
        let light_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("light pipeline"),
            layout: Some(&light_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_light"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_light"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: ACCUM_FORMAT,
                    // Additive: each light adds its contribution (Godot add).
                    blend: Some(wgpu::BlendState {
                        color: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::One,
                            operation: wgpu::BlendOperation::Add,
                        },
                        alpha: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::One,
                            operation: wgpu::BlendOperation::Add,
                        },
                    }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let shadow_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("shadow pipeline layout"),
            bind_group_layouts: &[Some(&shadow_bgl)],
            immediate_size: 0,
        });
        let shadow_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("shadow mask pipeline"),
            layout: Some(&shadow_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_shadow"),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: size_of::<[f32; 2]>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x2],
                })],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_shadow"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: SHADOW_MASK_FORMAT,
                    blend: None, // shadow fragments overwrite with 0
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let shadow_vb = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("shadow vertex buffer"),
            size: INITIAL_SHADOW_VERTS * size_of::<[f32; 2]>() as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            accum_texture,
            accum_view,
            shadow_mask_view,
            light_pipeline,
            shadow_pipeline,
            uniform,
            frame_bgl,
            frame_bind_group,
            shadow_bind_group,
            cookie_bgl,
            cookie_sampler,
            cookies: HashMap::new(),
            analytic_bind_group,
            shadow_vb,
            shadow_capacity: INITIAL_SHADOW_VERTS,
            verts: Vec::new(),
            plan: Vec::new(),
        }
    }

    /// Recreate the size-dependent targets (accumulation + shadow mask) at
    /// a new pixel size and rebind them against the caller's freshly
    /// resized MRT views. Pipelines, cookies, and buffers survive — only
    /// the editor's native-res scene path resizes; game scenes stay at the
    /// fixed logical size (PR-1).
    pub fn resize(
        &mut self,
        device: &wgpu::Device,
        normal_view: &wgpu::TextureView,
        mask_view: &wgpu::TextureView,
        width: u32,
        height: u32,
    ) {
        let (accum_texture, shadow_mask_view) = light_targets(device, width, height);
        self.frame_bind_group = frame_bind_group(
            device,
            &self.frame_bgl,
            &self.uniform,
            normal_view,
            mask_view,
            &shadow_mask_view,
        );
        self.accum_view = accum_texture.create_view(&wgpu::TextureViewDescriptor::default());
        self.accum_texture = accum_texture;
        self.shadow_mask_view = shadow_mask_view;
    }

    /// The light-accumulation target view (the composite pass samples it).
    pub fn accum_view(&self) -> &wgpu::TextureView {
        &self.accum_view
    }

    /// The light-accumulation texture (`COPY_SRC`, [`ACCUM_FORMAT`]) for
    /// readback.
    pub fn accum_texture(&self) -> &wgpu::Texture {
        &self.accum_texture
    }

    /// Upload a cookie texture under its asset `handle` (standalone —
    /// cookies are never atlased). Idempotent per handle. Lights referencing
    /// an unregistered cookie are skipped. An invalid texture is refused
    /// with a [`TextureError`].
    pub fn upload_cookie(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        handle: Handle<Texture>,
        tex: &Texture,
    ) -> Result<(), TextureError> {
        TextureError::check(device, tex)?;
        if self.cookies.contains_key(&handle) {
            return Ok(());
        }
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("light cookie"),
            size: wgpu::Extent3d {
                width: tex.width,
                height: tex.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            // NOT sRGB: cookies sample raw so the light math runs in gamma
            // space like Godot 2D (see the module docs / D-11).
            format: wgpu::TextureFormat::Rgba8Unorm,
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
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = cookie_bind_group(device, &self.cookie_bgl, &self.cookie_sampler, &view);
        self.cookies.insert(
            handle,
            Cookie {
                bind_group,
                size: Vec2::new(tex.width as f32, tex.height as f32),
            },
        );
        Ok(())
    }

    /// Plan this frame's lights: cull to the view, cap counts, fill the
    /// uniform, and build the shadow geometry buffer. Positions, footprints,
    /// heights and occluders are in the world camera's `units`. Call once
    /// per frame before [`run`](Self::run).
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view_proj: Mat4,
        units: WorldUnits,
        frame: &LightFrame,
        logical_size: (u32, u32),
    ) {
        self.plan.clear();
        self.verts.clear();

        // The world rectangle the camera shows (ortho: unproject the corners
        // — already in the camera's units, whatever its convention).
        let inv = view_proj.inverse();
        let mut view_min = Vec2::splat(f32::MAX);
        let mut view_max = Vec2::splat(f32::MIN);
        for (cx, cy) in [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
            let w = inv * Vec4::new(cx, cy, 0.5, 1.0);
            view_min = view_min.min(Vec2::new(w.x, w.y));
            view_max = view_max.max(Vec2::new(w.x, w.y));
        }

        let mut raw = [LightRaw::ZERO; MAX_LIGHTS];
        let mut shadow_count = 0usize;
        // Two passes over the lights: unshadowed first so `run` can batch
        // them into the single clearing pass (order is irrelevant: additive).
        for want_shadow in [false, true] {
            for light in &frame.lights {
                if self.plan.len() == MAX_LIGHTS {
                    break;
                }
                if light.shadows != want_shadow || light.energy <= 0.0 {
                    continue;
                }
                // The footprint half-extent and the shader's analytic
                // radius/falloff (radius 0 selects the cookie intensity; a
                // cookie light's own `falloff` is ignored and never uploaded).
                let (half, radius, falloff) = if let Some(handle) = light.cookie {
                    let Some(cookie) = self.cookies.get(&handle) else {
                        continue; // Cookie never uploaded — skip, never panic.
                    };
                    (
                        cookie_half_extent(cookie.size, light.texture_scale, units.pixels_per_unit),
                        0.0,
                        0.0,
                    )
                } else {
                    if !analytic_params_valid(light.radius, light.falloff) {
                        continue; // Degenerate shape — skip, never panic.
                    }
                    // `radius` is authored in the camera's units already.
                    (Vec2::splat(light.radius), light.radius, light.falloff)
                };
                if light.pos.x - half.x > view_max.x
                    || light.pos.x + half.x < view_min.x
                    || light.pos.y - half.y > view_max.y
                    || light.pos.y + half.y < view_min.y
                {
                    continue; // Off-screen.
                }

                let shadow = if light.shadows && shadow_count < MAX_SHADOW_LIGHTS {
                    let start = self.verts.len() as u32;
                    shadow_triangles(
                        light.pos,
                        half,
                        &frame.occluders,
                        // Always extrude past the footprint: an analytic
                        // radius is caller-chosen and may exceed SHADOW_FAR
                        // (in the camera's units).
                        (SHADOW_FAR / units.pixels_per_unit).max(2.0 * half.length()),
                        &mut self.verts,
                    );
                    let end = self.verts.len() as u32;
                    if end > start {
                        shadow_count += 1;
                        Some(start..end)
                    } else {
                        None // Nothing to occlude this light.
                    }
                } else {
                    None
                };

                let index = self.plan.len();
                let bits = mask_bits(light.item_mask);
                raw[index] = LightRaw {
                    pos_half: [light.pos.x, light.pos.y, half.x, half.y],
                    // Raw sRGB values: the light math runs in gamma space
                    // like Godot 2D (see the module docs).
                    color_energy: [light.color[0], light.color[1], light.color[2], light.energy],
                    params: [
                        light.height,
                        if shadow.is_some() { 1.0 } else { 0.0 },
                        radius,
                        falloff,
                    ],
                    mask: [bits[0], bits[1], bits[2], 0.0],
                };
                self.plan.push(LightDraw {
                    index: index as u32,
                    cookie: light.cookie,
                    shadow,
                });
            }
        }

        let uniform = LightsUniform {
            view_proj: view_proj.to_cols_array_2d(),
            screen: [
                logical_size.0 as f32,
                logical_size.1 as f32,
                units.screen_y_sign(),
                0.0,
            ],
            lights: raw,
        };
        queue.write_buffer(&self.uniform, 0, bytemuck::cast_slice(&[uniform]));

        if !self.verts.is_empty() {
            let needed = self.verts.len() as u64;
            if needed > self.shadow_capacity {
                let new_cap = needed.next_power_of_two();
                self.shadow_vb = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("shadow vertex buffer"),
                    size: new_cap * size_of::<[f32; 2]>() as u64,
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                self.shadow_capacity = new_cap;
            }
            queue.write_buffer(&self.shadow_vb, 0, bytemuck::cast_slice(&self.verts));
        }
    }

    /// The group-1 bind group for a planned draw: its cookie, or the shared
    /// white dummy for analytic lights.
    fn cookie_bind_group_for(&self, draw: &LightDraw) -> &wgpu::BindGroup {
        draw.cookie.map_or(&self.analytic_bind_group, |handle| {
            &self.cookies[&handle].bind_group
        })
    }

    /// Encode the light passes planned by [`prepare`](Self::prepare):
    /// clear the accumulation target and add every unshadowed light, then per
    /// shadowed light re-render the shared shadow mask and add the light.
    /// Shadow vertex-buffer capacity in vertices; grows only past it.
    #[cfg(all(test, not(target_arch = "wasm32")))]
    pub(crate) fn shadow_capacity(&self) -> u64 {
        self.shadow_capacity
    }

    pub fn run(&self, encoder: &mut wgpu::CommandEncoder) {
        // --- Clear + all unshadowed lights in one pass.
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("light accumulation pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.accum_view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.light_pipeline);
            pass.set_bind_group(0, &self.frame_bind_group, &[]);
            for draw in self.plan.iter().filter(|d| d.shadow.is_none()) {
                pass.set_bind_group(1, self.cookie_bind_group_for(draw), &[]);
                pass.draw(0..6, draw.index..draw.index + 1);
            }
        }

        // --- Shadowed lights: shadow mask pass + additive light pass each.
        for draw in &self.plan {
            let Some(range) = &draw.shadow else { continue };
            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("shadow mask pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &self.shadow_mask_view,
                        resolve_target: None,
                        depth_slice: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::WHITE),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                pass.set_pipeline(&self.shadow_pipeline);
                pass.set_bind_group(0, &self.shadow_bind_group, &[]);
                pass.set_vertex_buffer(0, self.shadow_vb.slice(..));
                pass.draw(range.clone(), 0..1);
            }
            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("shadowed light pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &self.accum_view,
                        resolve_target: None,
                        depth_slice: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Load,
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                pass.set_pipeline(&self.light_pipeline);
                pass.set_bind_group(0, &self.frame_bind_group, &[]);
                pass.set_bind_group(1, self.cookie_bind_group_for(draw), &[]);
                pass.draw(0..6, draw.index..draw.index + 1);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    // --- normal_factor -------------------------------------------------------

    /// A light directly above the fragment with a flat +Z normal is full
    /// strength.
    #[wasm_bindgen_test(unsupported = test)]
    fn normal_factor_overhead_flat_is_one() {
        let f = normal_factor(
            Vec2::new(10.0, 10.0),
            128.0,
            Vec2::new(10.0, 10.0),
            [0.0, 0.0, 1.0],
        );
        assert!((f - 1.0).abs() < 1e-6);
    }

    /// Offset by exactly `height` horizontally, a flat normal sees the light
    /// at 45°: factor = cos 45° = √2/2.
    #[wasm_bindgen_test(unsupported = test)]
    fn normal_factor_offset_flat_is_cos45() {
        let f = normal_factor(Vec2::ZERO, 128.0, Vec2::new(128.0, 0.0), [0.0, 0.0, 1.0]);
        assert!((f - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-5, "{f}");
    }

    /// A normal tilted *toward* the light beats the flat normal; tilted away
    /// clamps at 0.
    #[wasm_bindgen_test(unsupported = test)]
    fn normal_factor_tilt_toward_and_away() {
        let inv = std::f32::consts::FRAC_1_SQRT_2;
        // Light is to the left (-x); fragment normal tilted left = toward it.
        let toward = normal_factor(Vec2::ZERO, 128.0, Vec2::new(128.0, 0.0), [-inv, 0.0, inv]);
        assert!((toward - 1.0).abs() < 1e-5, "{toward}");
        let away = normal_factor(Vec2::ZERO, 1.0, Vec2::new(128.0, 0.0), [inv, 0.0, -inv]);
        assert_eq!(away, 0.0);
    }

    /// The "no normal map" encoding (degenerate normal) lights at full
    /// strength (Godot behavior for unmapped sprites).
    #[wasm_bindgen_test(unsupported = test)]
    fn normal_factor_degenerate_normal_is_one() {
        let f = normal_factor(Vec2::ZERO, 128.0, Vec2::new(500.0, 0.0), [0.0, 0.0, 0.0]);
        assert_eq!(f, 1.0);
    }

    /// Unnormalized normals are normalized before the dot.
    #[wasm_bindgen_test(unsupported = test)]
    fn normal_factor_normalizes_input() {
        let a = normal_factor(Vec2::ZERO, 128.0, Vec2::new(64.0, 32.0), [0.0, 0.0, 1.0]);
        let b = normal_factor(Vec2::ZERO, 128.0, Vec2::new(64.0, 32.0), [0.0, 0.0, 7.5]);
        assert!((a - b).abs() < 1e-6);
    }

    // --- analytic attenuation -------------------------------------------------

    /// Hand-evaluated `(1 − s²)² / (1 + falloff·s²)` (shadow-sp `REFERENCE_PORT` §6.4):
    /// full strength at the center; at `s = 0.5` with falloff 72,
    /// `0.75² / (1 + 18) = 0.5625 / 19 ≈ 0.029605`; zero at and beyond the
    /// radius.
    #[wasm_bindgen_test(unsupported = test)]
    fn analytic_attenuation_matches_hand_values() {
        assert_eq!(analytic_attenuation(0.0, 16.0, 72.0), 1.0);
        let mid = analytic_attenuation(8.0, 16.0, 72.0);
        assert!((mid - 0.029_605).abs() < 1e-5, "{mid}");
        assert_eq!(analytic_attenuation(16.0, 16.0, 72.0), 0.0);
        assert_eq!(analytic_attenuation(20.0, 16.0, 72.0), 0.0);
        // Falloff 0 leaves the bare bell: s = 0.5 → 0.75² = 0.5625.
        assert!((analytic_attenuation(8.0, 16.0, 0.0) - 0.5625).abs() < 1e-6);
        // Radius scales the curve: dist 4 in radius 8 is the same s = 0.5.
        assert_eq!(
            analytic_attenuation(4.0, 8.0, 72.0),
            analytic_attenuation(8.0, 16.0, 72.0)
        );
    }

    /// Degenerate shapes contribute nothing (and never divide by zero).
    #[wasm_bindgen_test(unsupported = test)]
    fn analytic_attenuation_rejects_degenerate_params() {
        assert_eq!(analytic_attenuation(0.0, 0.0, 72.0), 0.0);
        assert_eq!(analytic_attenuation(0.0, -16.0, 72.0), 0.0);
        assert_eq!(analytic_attenuation(0.0, f32::NAN, 72.0), 0.0);
        assert_eq!(analytic_attenuation(0.0, f32::INFINITY, 72.0), 0.0);
        assert_eq!(analytic_attenuation(0.0, 16.0, -1.0), 0.0);
        assert_eq!(analytic_attenuation(0.0, 16.0, f32::NAN), 0.0);
        assert!(!analytic_params_valid(16.0, f32::INFINITY));
        assert!(analytic_params_valid(16.0, 0.0));
    }

    /// A 512 px cookie at `texture_scale` 2 spans 1024 px: a ±512 px
    /// footprint under the pixel convention, ±16 units at 32 px/unit.
    #[wasm_bindgen_test(unsupported = test)]
    fn cookie_half_extent_is_in_camera_units() {
        assert_eq!(
            cookie_half_extent(Vec2::splat(512.0), 2.0, 1.0),
            Vec2::splat(512.0)
        );
        assert_eq!(
            cookie_half_extent(Vec2::splat(512.0), 2.0, 32.0),
            Vec2::splat(16.0)
        );
    }

    // --- masks ---------------------------------------------------------------

    #[wasm_bindgen_test(unsupported = test)]
    fn mask_bits_split_layers() {
        assert_eq!(mask_bits(1), [1.0, 0.0, 0.0]);
        assert_eq!(mask_bits(2), [0.0, 1.0, 0.0]);
        assert_eq!(mask_bits(4), [0.0, 0.0, 1.0]);
        assert_eq!(mask_bits(7), [1.0, 1.0, 1.0]);
        // Layers above 4 are unsupported and ignored.
        assert_eq!(mask_bits(8), [0.0, 0.0, 0.0]);
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn mask_matching_is_bit_intersection() {
        assert!(mask_matches(1, 1));
        assert!(mask_matches(7, 2));
        assert!(!mask_matches(1, 2));
        assert!(!mask_matches(4, 3));
        assert!(!mask_matches(8, 8), "layers above 4 never match");
    }

    /// Coverage dots the per-pixel layer coverage with the light's selectors
    /// and clamps: a layer-2 light ignores layer-1 coverage; multi-layer
    /// matches clamp to 1.
    #[wasm_bindgen_test(unsupported = test)]
    fn coverage_selects_and_clamps() {
        assert_eq!(coverage([1.0, 0.0, 0.0], 2), 0.0);
        assert_eq!(coverage([0.0, 0.75, 0.0], 2), 0.75);
        assert_eq!(coverage([1.0, 1.0, 1.0], 7), 1.0);
        assert_eq!(coverage([0.5, 0.5, 0.0], 3), 1.0);
    }

    // --- shadow extrusion -----------------------------------------------------

    fn square() -> Vec<Vec2> {
        vec![
            Vec2::new(0.0, 0.0),
            Vec2::new(32.0, 0.0),
            Vec2::new(32.0, 32.0),
            Vec2::new(0.0, 32.0),
        ]
    }

    fn point_in_tri(p: Vec2, a: Vec2, b: Vec2, c: Vec2) -> bool {
        let sign = |p1: Vec2, p2: Vec2, p3: Vec2| (p1 - p3).perp_dot(p2 - p3);
        let d1 = sign(p, a, b);
        let d2 = sign(p, b, c);
        let d3 = sign(p, c, a);
        let has_neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
        let has_pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
        !(has_neg && has_pos)
    }

    fn in_shadow(verts: &[[f32; 2]], p: Vec2) -> bool {
        verts.chunks_exact(3).any(|t| {
            point_in_tri(
                p,
                Vec2::from_array(t[0]),
                Vec2::from_array(t[1]),
                Vec2::from_array(t[2]),
            )
        })
    }

    /// A square occluder with the light to its left: points behind (right of)
    /// the square are shadowed, points beside or in front are lit, and the
    /// square's own interior is shadowed past the front face (Godot's "behind
    /// the first surface" shape).
    #[wasm_bindgen_test(unsupported = test)]
    fn shadow_covers_behind_the_occluder() {
        let light = Vec2::new(-50.0, 16.0);
        let mut verts = Vec::new();
        shadow_triangles(light, Vec2::splat(500.0), &[square()], 4096.0, &mut verts);
        assert!(!verts.is_empty());
        assert_eq!(verts.len() % 3, 0);

        assert!(in_shadow(&verts, Vec2::new(100.0, 16.0)), "behind the wall");
        assert!(in_shadow(&verts, Vec2::new(16.0, 16.0)), "wall interior");
        assert!(!in_shadow(&verts, Vec2::new(-20.0, 16.0)), "light side");
        assert!(!in_shadow(&verts, Vec2::new(16.0, -60.0)), "beside/above");
        assert!(!in_shadow(&verts, Vec2::new(16.0, 90.0)), "beside/below");
    }

    /// #319: a light 2 px from a long wall (the edge subtends nearly 180°)
    /// still shadows everything behind the wall inside its footprint: with a
    /// straight far edge, `(0, 200)` and `(0, 290)` were lit.
    #[wasm_bindgen_test(unsupported = test)]
    fn shadow_behind_a_long_wall_close_to_the_light_reaches_the_footprint() {
        let wall = vec![Vec2::new(-500.0, 2.0), Vec2::new(500.0, 10.0)];
        let half = Vec2::splat(300.0);
        for winding in [wall.clone(), wall.iter().rev().copied().collect()] {
            let mut verts = Vec::new();
            shadow_triangles(
                Vec2::ZERO,
                half,
                &[winding],
                2.0 * half.length(),
                &mut verts,
            );
            for p in [
                Vec2::new(0.0, 200.0),
                Vec2::new(0.0, 290.0),
                Vec2::new(290.0, 290.0),
            ] {
                assert!(in_shadow(&verts, p), "{p:?} behind the wall is lit");
            }
            for p in [
                Vec2::new(0.0, -200.0),
                Vec2::new(290.0, -290.0),
                Vec2::new(0.0, 1.0),
            ] {
                assert!(
                    !in_shadow(&verts, p),
                    "{p:?} on the light's side is shadowed"
                );
            }
        }
    }

    /// The result is winding-independent (all edges extrude; the union is the
    /// same region).
    #[wasm_bindgen_test(unsupported = test)]
    fn shadow_is_winding_independent() {
        let light = Vec2::new(-50.0, 16.0);
        let mut reversed = square();
        reversed.reverse();
        let mut a = Vec::new();
        let mut b = Vec::new();
        shadow_triangles(light, Vec2::splat(500.0), &[square()], 4096.0, &mut a);
        shadow_triangles(light, Vec2::splat(500.0), &[reversed], 4096.0, &mut b);
        for p in [
            Vec2::new(100.0, 16.0),
            Vec2::new(16.0, 16.0),
            Vec2::new(-20.0, 16.0),
            Vec2::new(16.0, -60.0),
        ] {
            assert_eq!(in_shadow(&a, p), in_shadow(&b, p), "{p:?}");
        }
    }

    /// Occluders outside the light's footprint AABB are culled entirely.
    #[wasm_bindgen_test(unsupported = test)]
    fn shadow_culls_far_occluders() {
        let mut verts = Vec::new();
        shadow_triangles(
            Vec2::new(1000.0, 1000.0),
            Vec2::splat(100.0), // footprint (900..1100)² — square at origin is far away
            &[square()],
            4096.0,
            &mut verts,
        );
        assert!(verts.is_empty());
    }

    /// Degenerate case: a vertex exactly on the light produces no NaN
    /// geometry (its edges are skipped).
    #[wasm_bindgen_test(unsupported = test)]
    fn shadow_skips_vertex_on_light() {
        let mut verts = Vec::new();
        shadow_triangles(
            Vec2::ZERO,
            Vec2::splat(500.0),
            &[square()],
            4096.0,
            &mut verts,
        );
        assert!(verts.iter().flatten().all(|c| c.is_finite()));
    }

    /// `LightFrame::default` is the identity: white modulate, nothing else.
    #[wasm_bindgen_test(unsupported = test)]
    fn default_light_frame_is_identity() {
        let frame = LightFrame::default();
        assert_eq!(frame.canvas_modulate, [1.0, 1.0, 1.0, 1.0]);
        assert!(frame.lights.is_empty());
        assert!(frame.occluders.is_empty());
    }
}

/// Headless GPU tests: the real light pipeline over small dummy targets,
/// read back and compared against hand-computed values.
#[cfg(all(test, not(target_arch = "wasm32")))]
mod gpu_tests {
    use super::*;
    use crate::canvas::camera::Camera;
    use crate::canvas::sprite::{MASK_FORMAT, NORMAL_FORMAT};
    use crate::canvas::test_gpu::{device, read_texture, rgba16f_to_f32};

    /// Target side in pixels (`Camera::new(SIZE, SIZE)` maps world pixel
    /// `(x, y)` to target pixel `(x, y)`).
    const SIZE: u32 = 64;

    /// A light pass over a receiver that covers every pixel on layer 1 with
    /// no normal map (flat normals, zero normal weight → `normal_factor` 1).
    struct Rig {
        device: wgpu::Device,
        queue: wgpu::Queue,
        pass: LightPass,
    }

    fn rig() -> Option<Rig> {
        let (device, queue) = device()?;
        let filled = |label: &str, format: wgpu::TextureFormat, texel: [u8; 4]| {
            device
                .create_texture_with_data(
                    &queue,
                    &wgpu::TextureDescriptor {
                        label: Some(label),
                        size: wgpu::Extent3d {
                            width: SIZE,
                            height: SIZE,
                            depth_or_array_layers: 1,
                        },
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format,
                        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                        view_formats: &[],
                    },
                    wgpu::util::TextureDataOrder::LayerMajor,
                    &texel.repeat((SIZE * SIZE) as usize),
                )
                .create_view(&wgpu::TextureViewDescriptor::default())
        };
        // Flat +Z normal, weight 0; full layer-1 coverage.
        let normal = filled("test normal target", NORMAL_FORMAT, [128, 128, 255, 0]);
        let mask = filled("test mask target", MASK_FORMAT, [255, 0, 0, 0]);
        let pass = LightPass::new(&device, &queue, &normal, &mask, SIZE, SIZE);
        Some(Rig {
            device,
            queue,
            pass,
        })
    }

    impl Rig {
        /// Plan + run `frame` and read the accumulation target back as
        /// `f32` rgba per pixel.
        fn render(&mut self, frame: &LightFrame) -> Vec<f32> {
            let view_proj = Camera::new(SIZE, SIZE).view_proj();
            self.pass.prepare(
                &self.device,
                &self.queue,
                view_proj,
                WorldUnits::default(),
                frame,
                (SIZE, SIZE),
            );
            let mut encoder = self
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
            self.pass.run(&mut encoder);
            self.queue.submit(Some(encoder.finish()));
            let bytes = read_texture(
                &self.device,
                &self.queue,
                self.pass.accum_texture(),
                SIZE,
                SIZE,
                8,
            );
            rgba16f_to_f32(&bytes)
        }
    }

    fn rgb(px: &[f32], x: u32, y: u32) -> [f32; 3] {
        let i = ((y * SIZE + x) * 4) as usize;
        [px[i], px[i + 1], px[i + 2]]
    }

    /// #319: an analytic light 2 px above a long wall leaves the pixel
    /// directly behind the wall, inside its radius, unlit, while a pixel as
    /// far away on the light's side is lit.
    #[test]
    fn a_light_close_to_a_long_wall_is_blocked_behind_it() {
        let Some(mut rig) = rig() else { return };
        let frame = LightFrame {
            lights: vec![PointLight {
                shadows: true,
                ..PointLight::analytic(Vec2::new(32.5, 20.5), 40.0, 0.0)
            }],
            occluders: vec![vec![
                Vec2::new(-1000.0, 22.5),
                Vec2::new(1000.0, 22.5),
                Vec2::new(1000.0, 24.5),
                Vec2::new(-1000.0, 24.5),
            ]],
            ..LightFrame::default()
        };
        let px = rig.render(&frame);
        assert_eq!(rgb(&px, 32, 44), [0.0; 3], "behind the wall");
        assert!(rgb(&px, 8, 20)[0] > 0.3, "the light's side is lit");
    }

    /// An analytic light centered on pixel (32, 32) with radius 16 and
    /// falloff 72 accumulates `energy × color × attenuation`: the center
    /// reads the full energy, 8 px out reads `0.029605 × energy`, 12 px out
    /// reads `0.19140625 / 41.5 × energy = 0.0046122 × energy`, the quad's
    /// corner region (inside the drawn square, beyond the radius) reads 0,
    /// and pixels outside the footprint are untouched.
    #[test]
    fn analytic_light_accumulates_the_falloff_curve() {
        let Some(mut rig) = rig() else { return };
        let frame = LightFrame {
            lights: vec![PointLight {
                color: [1.0, 0.5, 0.0],
                energy: 0.75,
                ..PointLight::analytic(Vec2::new(32.5, 32.5), 16.0, 72.0)
            }],
            ..LightFrame::default()
        };
        let px = rig.render(&frame);

        let close = |got: [f32; 3], want: [f32; 3], tol: f32| {
            got.iter().zip(want).all(|(g, w)| (g - w).abs() <= tol)
        };
        let center = rgb(&px, 32, 32);
        assert!(close(center, [0.75, 0.375, 0.0], 2e-3), "center {center:?}");
        let mid = rgb(&px, 40, 32);
        let want = 0.029_605 * 0.75;
        assert!(close(mid, [want, want * 0.5, 0.0], 3e-4), "s=0.5 {mid:?}");
        let far = rgb(&px, 32, 44);
        let want = 0.004_612_2 * 0.75;
        assert!(close(far, [want, want * 0.5, 0.0], 1e-4), "s=0.75 {far:?}");
        // dist = 14√2 ≈ 19.8 > radius, but inside the ±16 quad.
        assert_eq!(rgb(&px, 46, 46), [0.0; 3], "quad corner past the radius");
        assert_eq!(rgb(&px, 32, 52), [0.0; 3], "outside the footprint");
    }

    /// A cookie light is unaffected by the analytic path: a 1×1 cookie of
    /// `rgba(255, 255, 255, 128)` scaled to a 32 px footprint reads
    /// `128/255 × energy` everywhere inside the footprint — including the
    /// corner region an analytic light would leave dark — and 0 outside.
    #[test]
    fn cookie_light_samples_the_cookie_everywhere_in_its_footprint() {
        let Some(mut rig) = rig() else { return };
        let mut assets = crate::assets::Assets::<Texture>::new();
        let handle = assets.insert(
            "cookie.png".into(),
            Texture {
                width: 1,
                height: 1,
                rgba: vec![255, 255, 255, 128],
            },
        );
        // #320: an empty cookie is refused and does not claim the handle,
        // so the valid upload below still registers it.
        let empty = Texture {
            width: 0,
            height: 1,
            rgba: Vec::new(),
        };
        assert_eq!(
            rig.pass
                .upload_cookie(&rig.device, &rig.queue, handle, &empty),
            Err(TextureError::Empty {
                width: 0,
                height: 1
            })
        );
        let tex = assets.get(handle).expect("just inserted");
        rig.pass
            .upload_cookie(&rig.device, &rig.queue, handle, tex)
            .unwrap();
        let frame = LightFrame {
            lights: vec![PointLight {
                texture_scale: 32.0,
                energy: 0.5,
                ..PointLight::new(handle, Vec2::new(32.5, 32.5))
            }],
            ..LightFrame::default()
        };
        let px = rig.render(&frame);
        let want = 128.0 / 255.0 * 0.5;
        for (x, y) in [(32, 32), (40, 32), (46, 46), (17, 17)] {
            let got = rgb(&px, x, y);
            assert!(
                got.iter().all(|c| (c - want).abs() < 2e-3),
                "({x}, {y}) = {got:?}, want {want}"
            );
        }
        assert_eq!(rgb(&px, 32, 52), [0.0; 3], "outside the footprint");
    }

    /// Degenerate analytic shapes and unknown cookies are skipped in
    /// `prepare` — nothing is planned, nothing is drawn, nothing panics.
    #[test]
    fn invalid_lights_are_skipped_without_panicking() {
        let Some(mut rig) = rig() else { return };
        let center = Vec2::new(32.5, 32.5);
        let frame = LightFrame {
            lights: vec![
                PointLight::analytic(center, 0.0, 72.0),
                PointLight::analytic(center, -16.0, 72.0),
                PointLight::analytic(center, f32::NAN, 72.0),
                PointLight::analytic(center, f32::INFINITY, 72.0),
                PointLight::analytic(center, 16.0, -1.0),
                PointLight::analytic(center, 16.0, f32::NAN),
            ],
            ..LightFrame::default()
        };
        let px = rig.render(&frame);
        assert!(rig.pass.plan.is_empty());
        // The pass still clears the target (rgb 0; alpha is the clear's 1).
        assert!(px.chunks_exact(4).all(|p| p[..3] == [0.0; 3]));
    }
}
