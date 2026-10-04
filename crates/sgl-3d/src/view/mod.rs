//! Views and what they see. The main camera, each directional shadow
//! cascade, each local-light shadow face and each probe-capture face is a
//! `View` with its own `ViewUniform`; a `DrawList` built from the scene and a
//! view holds its culled, LOD-selected, instanced draws, `Clusters` the
//! scene lights and decals that reach each part of it, and `GeometryPipelines` the
//! pipelines they draw with. `cascades` fits the directional shadow's cascades, and
//! `frame_uniform` builds the frame data every view of a frame shares,
//! including the cascades.
//!
//! The types stages share, which the renderer owns and lends them, are
//! here too: the frame's context (`frame`), the effective configuration
//! (`effective`), the sizes and shared targets (`targets`), group 0
//! (`bindings`) and DiligentFX's post-effect context (`post_fx`).
pub(crate) mod bindings;
pub(crate) mod cascades;
pub(crate) mod clusters;
pub(crate) mod culling;
pub(crate) mod draw_list;
pub(crate) mod effective;
pub(crate) mod frame;
pub(crate) mod history;
pub(crate) mod lod;
pub(crate) mod pipelines;
pub(crate) mod population;
pub(crate) mod post_fx;
pub(crate) mod reflection_camera;
pub(crate) mod targets;

use crate::FrameInput;
use crate::content::lighting::{Backdrop, DirectionalLight, DirectionalShadow};
use crate::scene::static_lighting::StaticLighting;
use crate::shading::uniforms::{
    DIRECTIONAL_LIGHT_SHADOW, DirectionalLightUniform, FRAME_BACKDROP_COLOR, FRAME_BAKED_LIGHTING,
    FRAME_FOG, FRAME_HARDWARE_SHADOW_FILTER, FRAME_IRRADIANCE_ATLAS, FRAME_TEMPORAL_SHADOW_FILTER,
    FrameUniform, ShadowCascadeUniform, VIEW_PROBE_CAPTURE, ViewUniform,
};
use bytemuck::Zeroable;
use cascades::{Cascades, MAX_SHADOW_CASCADES};
use effective::ShadowFilter;
use glam::camera;
use glam::{Mat4, Vec3};

/// The near plane of local-light shadow faces, in metres.
pub(crate) const LOCAL_SHADOW_NEAR: f32 = 0.02;
/// Each local-light shadow cube face's direction and up, in face order.
const LOCAL_SHADOW_FACES: [(Vec3, Vec3); 6] = [
    (Vec3::X, Vec3::NEG_Y),
    (Vec3::NEG_X, Vec3::NEG_Y),
    (Vec3::Y, Vec3::Z),
    (Vec3::NEG_Y, Vec3::NEG_Z),
    (Vec3::Z, Vec3::NEG_Y),
    (Vec3::NEG_Z, Vec3::NEG_Y),
];
/// Each probe-capture face's direction and up, in cube-face order.
const PROBE_FACES: [(Vec3, Vec3); 6] = [
    (Vec3::X, Vec3::Y),
    (Vec3::NEG_X, Vec3::Y),
    (Vec3::Y, Vec3::Z),
    (Vec3::NEG_Y, Vec3::NEG_Z),
    (Vec3::NEG_Z, Vec3::Y),
    (Vec3::Z, Vec3::Y),
];

fn flag(on: bool, flag: u32) -> u32 {
    if on { flag } else { 0 }
}

/// Directional light `index` of `input`, unless it is absent or off.
fn directional_light(input: &FrameInput, index: usize) -> Option<DirectionalLight> {
    input.directional_lights[index].filter(DirectionalLight::is_on)
}

/// The directional light that casts the frame's shadow, by its index in
/// `FrameInput::directional_lights`, with its shadow: the first light that
/// is on and has one.
pub(crate) fn directional_shadow(input: &FrameInput) -> Option<(usize, DirectionalShadow)> {
    (0..input.directional_lights.len()).find_map(|index| {
        directional_light(input, index)?
            .shadow
            .map(|shadow| (index, shadow))
    })
}

/// The directional shadow of one frame's views: the light that casts it and
/// its cascades, the camera's filter, and the frames since history
/// restarted, which turn the temporal filter's noise.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct FrameShadow {
    pub light: Option<usize>,
    pub cascades: Cascades,
    pub filter: ShadowFilter,
    pub frame_count: u32,
}

impl FrameShadow {
    /// The shadow `input`'s camera sees in cascades of `map_size` texels.
    /// Its surfaces take every shadow, local lights' too, with `filter`,
    /// whether or not a directional light casts one.
    pub fn camera(
        input: &FrameInput,
        map_size: u32,
        filter: ShadowFilter,
        frame_count: u32,
    ) -> Self {
        let camera = input.camera;
        let (light, cascades) = Self::fit(input, |direction, shadow| {
            Cascades::camera(camera.view, camera.projection, direction, shadow, map_size)
        })
        .map_or((None, Cascades::default()), |(light, cascades)| {
            (Some(light), cascades)
        });
        Self {
            light,
            cascades,
            filter,
            frame_count,
        }
    }

    /// The shadow of `input`'s light for a probe capture at `center`, in
    /// cascades of `map_size` texels.
    pub fn capture(input: &FrameInput, center: Vec3, map_size: u32) -> Self {
        Self::fit(input, |direction, shadow| {
            Cascades::capture(center, direction, shadow, map_size)
        })
        .map_or_else(Self::default, |(light, cascades)| Self {
            light: Some(light),
            cascades,
            ..Self::default()
        })
    }

    /// The casting light's index and its cascades, fit by `fit` from its
    /// direction and shadow.
    fn fit(
        input: &FrameInput,
        fit: impl FnOnce(Vec3, &DirectionalShadow) -> Cascades,
    ) -> Option<(usize, Cascades)> {
        let (light, shadow) = directional_shadow(input)?;
        let direction = input.directional_lights[light]?.direction;
        Some((light, fit(direction, &shadow)))
    }
}

/// The frame data of `input` for a scene with `baked` diffuse lighting, with
/// the directional `shadow`; `fog` is whether the frame's volumetric fog ran,
/// so draws fog from its volume.
pub(crate) fn frame_uniform(
    input: &FrameInput,
    baked: &StaticLighting,
    shadow: &FrameShadow,
    fog: bool,
) -> FrameUniform {
    let cascades = shadow.cascades.as_slice();
    let directional_lights = std::array::from_fn(|index| {
        directional_light(input, index).map_or(DirectionalLightUniform::zeroed(), |light| {
            DirectionalLightUniform {
                direction_to_light: (-light.direction).to_array(),
                flags: flag(shadow.light == Some(index), DIRECTIONAL_LIGHT_SHADOW),
                color: light.color,
                illuminance: light.illuminance,
                // As `is_on` takes a light it cannot shine for none, a fog
                // energy the fog cannot scale by leaves it out of the fog.
                fog_energy: if light.fog_energy.is_finite() && light.fog_energy > 0. {
                    light.fog_energy
                } else {
                    0.
                },
                // A shadow opacity the blend cannot take draws no shadow.
                shadow_opacity: if light.shadow_opacity.is_finite() {
                    light.shadow_opacity.clamp(0., 1.)
                } else {
                    0.
                },
                padding: [0.; 2],
            }
        })
    });
    let shadow_cascades: [ShadowCascadeUniform; MAX_SHADOW_CASCADES] =
        std::array::from_fn(|index| {
            cascades
                .get(index)
                .map_or(ShadowCascadeUniform::zeroed(), |cascade| {
                    ShadowCascadeUniform {
                        clip_from_world: cascade.clip_from_world.to_cols_array_2d(),
                        texel_size: cascade.texel_size,
                        far_bound: cascade.far_bound,
                        padding: [0.; 2],
                    }
                })
        });
    let (backdrop_color, backdrop_yaw, backdrop_brightness) = match input.backdrop {
        Backdrop::Environment { yaw, brightness } => ([0.; 3], yaw, brightness),
        Backdrop::Color(color) => (color, 0., 0.),
    };
    let hemisphere = input.hemisphere_light;
    let mist = input.mist;
    FrameUniform {
        directional_lights,
        shadow_cascades,
        hemisphere_sky_color: hemisphere.sky_color,
        hemisphere_intensity: hemisphere.intensity,
        hemisphere_ground_color: hemisphere.ground_color,
        diffuse_environment_yaw: input.diffuse_environment.yaw,
        backdrop_color,
        diffuse_environment_intensity: input.diffuse_environment.intensity,
        mist_thin_color: mist.thin_color,
        mist_opacity: mist.opacity,
        mist_dense_color: mist.dense_color,
        backdrop_yaw,
        mist_size: [mist.width, mist.height],
        mist_drift: mist.drift,
        backdrop_brightness,
        fog_inverse_length: input.fog.length.recip(),
        fog_inverse_detail_spread: crate::shading::fog::DETAIL_SPREAD.recip(),
        reflection_yaw: input.reflection_environment.yaw,
        reflection_intensity: input.reflection_environment.intensity,
        elapsed_seconds: input.elapsed_seconds,
        fixed_irradiance_scale: baked.atlas_scale,
        visibility_mask: input.visibility_mask,
        lightmap_chart: baked.lightmap_chart,
        flags: flag(fog, FRAME_FOG)
            | flag(input.baked_lighting, FRAME_BAKED_LIGHTING)
            | flag(baked.atlas_installed, FRAME_IRRADIANCE_ATLAS)
            | flag(
                matches!(input.backdrop, Backdrop::Color(_)),
                FRAME_BACKDROP_COLOR,
            )
            | flag(
                shadow.filter == ShadowFilter::Temporal,
                FRAME_TEMPORAL_SHADOW_FILTER,
            )
            | flag(
                shadow.filter == ShadowFilter::Hardware,
                FRAME_HARDWARE_SHADOW_FILTER,
            ),
        shadow_cascade_count: cascades.len() as u32,
        frame_count: shadow.frame_count,
        padding: 0,
    }
}

/// The jitter antialiasing applies to a view's raster projection: an NDC
/// offset, and the texture mip bias that goes with it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Jitter {
    pub ndc: [f32; 2],
    pub mip_bias: f32,
}

/// One view: the view data it renders and culls with. Each kind of view has
/// its constructor.
#[derive(Clone, Copy)]
pub(crate) struct View {
    pub uniform: ViewUniform,
}

impl View {
    /// The main camera, with the view data its frame uploaded.
    pub fn camera(uniform: ViewUniform) -> Self {
        Self { uniform }
    }

    /// Face `face` (0..6) of a static specular probe captured at `center`.
    /// Faces use right-handed views stored as a cube mirrored in world Z, so
    /// scene winding is unchanged; lookups flip Z back.
    pub fn probe_face(center: Vec3, face: u8) -> Self {
        let (direction, up) = PROBE_FACES[usize::from(face)];
        let projection = crate::perspective(std::f32::consts::FRAC_PI_2, 1., 0.1);
        let view = camera::rh::view::look_at_mat4(center, center + direction, up);
        let view_projection = projection * view;
        Self {
            uniform: ViewUniform {
                view: view.to_cols_array_2d(),
                projection: projection.to_cols_array_2d(),
                view_projection: view_projection.to_cols_array_2d(),
                inverse_view_projection: view_projection.inverse().to_cols_array_2d(),
                stable_view_projection: view_projection.to_cols_array_2d(),
                previous_view_projection: view_projection.to_cols_array_2d(),
                eye: center.to_array(),
                mip_bias: 0.,
                jitter: [0.; 2],
                viewport: [0.; 2],
                flags: VIEW_PROBE_CAPTURE,
                padding: [0; 3],
            },
        }
    }

    /// A directional shadow cascade with reversed-Z orthographic
    /// `clip_from_world`: an identity view with the cascade's matrix as the
    /// projection.
    pub fn shadow_cascade(clip_from_world: Mat4) -> Self {
        let clip = clip_from_world.to_cols_array_2d();
        Self {
            uniform: ViewUniform {
                view: Mat4::IDENTITY.to_cols_array_2d(),
                projection: clip,
                view_projection: clip,
                ..ViewUniform::zeroed()
            },
        }
    }

    /// Face `face` (0..6) of the local-light shadow cube of a light at
    /// `position`, reaching `range` metres: a right-handed 90° view along
    /// the face's axis, reversed-Z out to the range.
    pub fn local_shadow_face(face: u8, position: Vec3, range: f32) -> Self {
        let (direction, up) = LOCAL_SHADOW_FACES[usize::from(face)];
        Self::local_shadow(position, direction, up, std::f32::consts::FRAC_PI_2, range)
    }

    /// The local-light shadow face of a spot light at `position` shining
    /// along unit `direction`, its cone `outer_angle` wide and reaching
    /// `range` metres: a view along its direction covering the cone, as
    /// Bevy's and Godot's spot shadows are, reversed-Z out to the range.
    pub fn spot_shadow(position: Vec3, direction: Vec3, outer_angle: f32, range: f32) -> Self {
        let up = if direction.y.abs() < 0.9 {
            Vec3::Y
        } else {
            Vec3::X
        };
        Self::local_shadow(position, direction, up, 2. * outer_angle, range)
    }

    fn local_shadow(position: Vec3, direction: Vec3, up: Vec3, fov: f32, range: f32) -> Self {
        let projection = camera::rh::proj::directx::perspective(fov, 1., range, LOCAL_SHADOW_NEAR);
        let view = camera::rh::view::look_to_mat4(position, direction, up);
        Self {
            uniform: ViewUniform {
                view: view.to_cols_array_2d(),
                projection: projection.to_cols_array_2d(),
                view_projection: (projection * view).to_cols_array_2d(),
                eye: position.to_array(),
                ..ViewUniform::zeroed()
            },
        }
    }

    pub fn view_projection(&self) -> Mat4 {
        Mat4::from_cols_array_2d(&self.uniform.view_projection)
    }

    /// The clip volume an object at `pose` is rasterized against, with the
    /// view's jitter.
    pub fn frustum(&self, pose: Mat4) -> culling::Frustum {
        culling::Frustum::new(
            Mat4::from_cols_array_2d(&self.uniform.view),
            Mat4::from_cols_array_2d(&self.uniform.projection),
            pose,
            self.uniform.jitter,
        )
    }
}

/// One view of a frame: its view data, the uniform buffer group 0 binds it
/// from, and its draws.
pub(crate) struct ViewSlot {
    pub view: View,
    pub buffer: wgpu::Buffer,
    pub list: draw_list::DrawList,
}

impl ViewSlot {
    fn new(device: &wgpu::Device, label: &str) -> Self {
        let view = View {
            uniform: ViewUniform::zeroed(),
        };
        Self {
            buffer: crate::scene::buffer(
                device,
                label,
                bytemuck::bytes_of(&view.uniform),
                wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            ),
            view,
            list: draw_list::DrawList::default(),
        }
    }

    /// Sets this slot's view and uploads its uniform.
    pub fn set(&mut self, queue: &wgpu::Queue, view: View) {
        self.view = view;
        queue.write_buffer(&self.buffer, 0, bytemuck::bytes_of(&view.uniform));
    }
}

/// The views of one frame and what each sees, built once per frame by the
/// prepare stage and read by every later stage: the camera and its
/// clusters, the light and decal lists of world-space ray hits and the
/// directional shadow's cascades, and the draw instances of every list the
/// frame draws.
pub(crate) struct FrameViews {
    pub camera: ViewSlot,
    /// The camera's blended surfaces, back to front; `camera.list` holds
    /// its opaque and masked ones.
    pub blended: draw_list::DrawList,
    /// The scene lights and decals that reach each of the camera's
    /// clusters.
    pub clusters: clusters::Clusters,
    /// The scene lights and decals world-space ray hits shade with.
    pub ray_lists: clusters::Clusters,
    /// Each directional shadow cascade, nearest first; the frame uses the
    /// first `cascade_count`.
    pub cascades: [ViewSlot; MAX_SHADOW_CASCADES],
    pub cascade_count: usize,
    /// The camera as reflections and DiligentFX see it: its view and its
    /// projection with the frame's jitter.
    pub reflection_camera: reflection_camera::Camera,
    /// Every draw list's instances this frame: these views' and the
    /// local-light shadow faces'.
    pub instances: draw_list::DrawInstances,
}

impl FrameViews {
    pub fn new(device: &wgpu::Device) -> Self {
        Self {
            camera: ViewSlot::new(device, "camera view"),
            blended: draw_list::DrawList::default(),
            clusters: clusters::Clusters::new(device, "camera clusters"),
            ray_lists: clusters::Clusters::new(device, "ray hit lights and decals"),
            cascades: std::array::from_fn(|_| ViewSlot::new(device, "directional shadow cascade")),
            cascade_count: 0,
            reflection_camera: reflection_camera::Camera::new(Mat4::IDENTITY, Mat4::IDENTITY),
            instances: draw_list::DrawInstances::default(),
        }
    }
}
