//! Raw GPU observations for renderer integration tests and diagnostics
//! (feature `diagnostics`): the frame's intermediate targets and readback,
//! and what the library counted on this thread (`counters`): its uploads by
//! call site, buffers created, build steps' times, ray-source growths,
//! static-edit boxes and acceleration-structure builds.
use crate::InstanceId;
use crate::content::identity::Identity;
pub use crate::counters::{BuildStep, Counters, StepTime, UploadSite};
pub use crate::scene::SceneResources;
pub use crate::stages::dynamic_gi::DynamicGiChanges;

/// What the library counted on this thread since it started. Take two and
/// `Counters::since` for what a frame or an operation cost.
///
/// It counts `sgl-3d`'s own uploads and buffers only: the constant and
/// staging buffers `sgl-post-fx` and the FSR2 port (`sp-fidelity`) write
/// for their passes are not included.
pub fn counters() -> Counters {
    crate::counters::snapshot()
}

/// The draws the last frame's camera and directional-cascade views encoded
/// (`Renderer::diagnostic_draws`): one per instanced draw call of the
/// CPU-built blended list, and one per set and phase of the GPU-built
/// lists, an indirect draw of the sections the GPU appended, with a
/// cascade's second per set, the indexed draw of its sections whose
/// triangles pair. Local-light shadow faces, probe captures and full-screen
/// passes are not counted; the local-light shadow atlas's draws are
/// `LocalShadowStats::draws`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ViewDraws {
    /// The camera's opaque and masked surfaces, and its blended ones.
    pub camera: usize,
    pub blended: usize,
    /// Each directional shadow cascade, nearest first.
    pub cascades: Vec<usize>,
}

/// The CPU time the last frame's camera and directional-cascade views took
/// (`Renderer::diagnostic_view_times`): the camera's opaque and masked draw
/// list, and each cascade's, nearest first.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ViewTimes {
    pub camera: ViewTime,
    pub cascades: Vec<ViewTime>,
}

/// One view's CPU time in a frame, in milliseconds: building its GPU-built
/// draw list (preparing its cull and encoding its reset and cull
/// dispatches), and recording its passes' draws from it into the game's
/// encoder, from each pass's start to its end. On WebGPU, recording issues each command to the
/// browser; natively, wgpu validates and encodes a pass's commands when the
/// game finishes the encoder, which this does not include.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ViewTime {
    pub build_ms: f64,
    pub encode_ms: f64,
}

/// One frame of the dynamic GI stage, observed (`Diagnostics::dynamic_gi`,
/// `Renderer::take_dynamic_gi_reports`). A probe's rays are those its
/// irradiance takes; its fixed rays classify it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DynamicGiReport {
    /// The volume's probes, those that traced rays, and those not yet
    /// blended.
    pub probes: u32,
    pub traced_probes: u32,
    pub unblended_probes: u32,
    /// The rays, fixed rays included, the blended probes asked for on
    /// their turns at their own periods, and the stride the frame
    /// lengthened every period by so they fit the frame's budget beside the
    /// probes that start.
    pub blended_requests: u32,
    pub stride: u32,
    /// The probes that traced, by how many rays each traced beside its
    /// fixed rays: 4, 5–8, 9–16, 17–32, 33–64, 65–128, 129–255 and 256.
    pub probes_by_rays: [u32; 8],
    /// The rays and fixed rays traced, and those of them that hit.
    pub rays: u32,
    pub fixed_rays: u32,
    pub hits: u32,
    /// The visibility rays the rays' hits cast toward the light each drew.
    pub visibility_rays: u32,
    /// The BVH nodes the rays and fixed rays visited, and the most one of
    /// them visited.
    pub ray_visits: u64,
    pub most_ray_visits: u32,
    /// The BVH nodes the visibility rays visited, and the most one visited.
    pub visibility_visits: u64,
    pub most_visibility_visits: u32,
    /// The queries of either kind that stopped at the most nodes a ray
    /// visits (AR-12), or at a link that does not lead forward, and so
    /// reported a miss.
    pub exhausted_queries: u32,
    /// Whether the volume paused, tracing nothing, having converged while
    /// what its light follows held still, and whether it has converged
    /// after the frame.
    pub paused: bool,
    pub converged: bool,
    /// What its light follows that changed since the last frame that ran
    /// it.
    pub changes: DynamicGiChanges,
    /// The whole spacings it scrolled by since that frame.
    pub scrolled: [i32; 3],
    /// The moving instances' bounds about which its probes trace as active
    /// ones.
    pub moving_bounds: u32,
}

/// The value `DiagnosticTarget::SourceId`'s R channel holds for `instance`'s
/// pixels in a frame rendered while the scene had it.
pub fn source_id(instance: InstanceId) -> u32 {
    instance.index() as u32 + 1
}

/// The perceptual roughness above which `ReflectionMethod::Crystal` traces no
/// ray; its reflections fade out over the 0.05 below it.
pub fn crystal_roughness_threshold() -> f32 {
    crate::view::post_fx::ssr_attribs().roughness_threshold
}

/// An intermediate target of the last rendered frame
/// (`Renderer::diagnostic_target`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiagnosticTarget {
    /// Depth32Float, reversed-Z.
    Depth,
    /// Rgba16Float: signed octahedral world-space base normal in RG and coat
    /// normal in BA.
    Normal,
    /// Rg16Float: current minus previous unjittered UV, +y down.
    Motion,
    /// Rg32Uint: each pixel's raster source in R (0 none, else the
    /// instance's [`source_id`]) and primitive in G.
    SourceId,
    /// The linear HDR scene after reflections and transparent effects.
    Composite,
    /// R32Uint: XeGTAO visibility in 0..=255, when ambient occlusion ran.
    AmbientOcclusion,
    /// Rgba16Float at the output size: the tone-mapped scene before
    /// presentation, when the frame captured it
    /// (`Diagnostics::capture_tone_target` or `frame_probe`).
    ToneMapped,
    /// R32Float, 1×1: the frame's exposure multiplier, which the tone map
    /// and FSR2 read.
    Exposure,
}

pub fn read(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    bpp: u32,
) -> Vec<u8> {
    let size = texture.size();
    let row = (size.width * bpp).div_ceil(256) * 256;
    let buffer = crate::counters::buffer(
        device,
        &wgpu::BufferDescriptor {
            label: Some("frame evidence"),
            size: u64::from(row * size.height),
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        },
    );
    let mut encoder = device.create_command_encoder(&Default::default());
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row),
                rows_per_image: Some(size.height),
            },
        },
        size,
    );
    queue.submit([encoder.finish()]);
    let (tx, rx) = std::sync::mpsc::channel();
    buffer.map_async(wgpu::MapMode::Read, .., move |r| {
        tx.send(r).unwrap();
    });
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    rx.recv().unwrap().unwrap();
    let mapped = buffer.get_mapped_range(..);
    let mut pixels = Vec::new();
    for line in mapped.chunks(row as usize) {
        pixels.extend_from_slice(&line[..(size.width * bpp) as usize]);
    }
    pixels
}
pub fn half(bytes: &[u8]) -> f32 {
    let bits = u16::from_le_bytes([bytes[0], bytes[1]]);
    let sign = if bits & 0x8000 == 0 { 1.0 } else { -1.0 };
    let exponent = ((bits >> 10) & 31) as i32;
    let fraction = (bits & 1023) as f32;
    sign * if exponent == 0 {
        fraction * 2f32.powi(-24)
    } else if exponent == 31 {
        if fraction == 0. {
            f32::INFINITY
        } else {
            f32::NAN
        }
    } else {
        (1. + fraction / 1024.) * 2f32.powi(exponent - 15)
    }
}
