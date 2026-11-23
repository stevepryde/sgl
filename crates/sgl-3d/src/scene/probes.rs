//! The scene's specular probe collection on the GPU: the installed baked
//! probes (`baked_specular_probe`'s content), then the live ones
//! (`live`), in one cube array with one metadata buffer, and
//! `Scene::set_baked_specular_probes`.
mod cubes;
mod live;

use super::probe_grid::{ProbeGrid, REACH};
use super::{Scene, buffer};
use crate::baked_specular_probe::{
    BakedSpecularProbe, LEVELS, MAX_PROBES, ProbeError, SpecularProbeBox, SpecularProbeTexels,
};
use crate::content::live_specular_probe::MAX_LIVE_PROBES;
pub(crate) use cubes::{copy_cubes, payload_size};
use cubes::{cube_array, cube_array_view, upload};
use glam::{Mat3, Mat4, Vec3};
use live::Live;

impl SpecularProbeTexels {
    fn format(&self) -> wgpu::TextureFormat {
        match self {
            Self::Rgba16Float(_) => wgpu::TextureFormat::Rgba16Float,
            Self::Bc6hUfloat(_) => wgpu::TextureFormat::Bc6hRgbUfloat,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ProbeMetadata {
    world_to_local: [[f32; 4]; 4],
    /// The capture position in the probe's frame; w is 1 when the proxy
    /// corrects parallax.
    center: [f32; 4],
    influence_min: [f32; 4],
    influence_max: [f32; 4],
    blend: [f32; 4],
    proxy_min: [f32; 4],
    proxy_max: [f32; 4],
    sphere: [f32; 4],
}

impl ProbeMetadata {
    /// A probe no receiver is inside: a live probe before its first
    /// publication, whose cube holds nothing yet. Its influence box is
    /// empty, so `probe_influence` weighs it zero everywhere.
    fn nowhere() -> Self {
        Self {
            world_to_local: Mat4::IDENTITY.to_cols_array_2d(),
            influence_min: [1., 1., 1., 0.],
            influence_max: [-1., -1., -1., 0.],
            ..bytemuck::Zeroable::zeroed()
        }
    }
}

// One storage buffer: fixed arrays keep host and WGSL indexing bounded
// independently of caller data. The world grid's words follow it in the
// same buffer (`ProbeCollection::grid`). Baked probes' records never change;
// a live probe's is written in place when it moves or publishes.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CollectionMetadata {
    /// Every probe, then the baked ones, which come first.
    counts: [u32; 4],
    /// `probe_grid::ProbeGrid`'s origin, scale and size.
    grid_origin: [f32; 3],
    grid_scale: f32,
    grid_size: [u32; 3],
    padding: u32,
    probes: [ProbeMetadata; MAX_PROBES],
}

impl CollectionMetadata {
    /// Zeroed on the heap and filled in place: at about 45 KB, building or
    /// moving it on the stack overflows `View::new` in a debug WASM build
    /// (#839).
    fn new(grid: &ProbeGrid) -> Box<Self> {
        let mut metadata: Box<Self> = bytemuck::zeroed_box();
        metadata.grid_origin = grid.origin.to_array();
        metadata.grid_scale = grid.scale;
        metadata.grid_size = grid.size.to_array();
        metadata
    }
}

pub(crate) struct UploadedProbes {
    pub view: wgpu::TextureView,
    /// The collection's metadata, then the world grid's cells and their
    /// probe lists (`ProbeGrid::words`).
    pub metadata: wgpu::Buffer,
    /// Each baked probe's placement, which the metadata and grid are built
    /// from.
    placements: Vec<Placement>,
    /// The live probes, whose cubes and records follow the baked ones' in
    /// this order.
    live: Vec<Live>,
    /// What every cube of the array is.
    face_size: u32,
    format: wgpu::TextureFormat,
}

/// Where a probe is: what its metadata and the world grid hold of it.
#[derive(Clone, Copy)]
struct Placement {
    /// The capture position in the probe's frame, which a render origin
    /// move leaves unchanged.
    center: Vec3,
    world_to_local: Mat4,
    influence: SpecularProbeBox,
    blend: Vec3,
    proxy: Option<SpecularProbeBox>,
}

/// A rigid `world_to_local` in a render frame whose origin lies `by` from
/// this one's: its frame's origin translated, and the transform rebuilt
/// from that origin rather than translated.
fn translated_frame(world_to_local: Mat4, by: Vec3) -> Mat4 {
    let rotation = Mat3::from_mat4(world_to_local);
    let origin = -(rotation.transpose() * world_to_local.w_axis.truncate()) - by;
    let mut moved = world_to_local;
    moved.w_axis = (-(rotation * origin)).extend(world_to_local.w_axis.w);
    moved
}

impl Placement {
    fn new(probe: &BakedSpecularProbe) -> Self {
        Self {
            center: probe.world_to_local.transform_point3(probe.center),
            world_to_local: probe.world_to_local,
            influence: probe.influence,
            blend: probe.blend,
            proxy: probe.proxy,
        }
    }

    /// The placement in a render frame whose origin lies `by` from this
    /// one's (`translated_frame`).
    fn translated(self, by: Vec3) -> Self {
        Self {
            world_to_local: translated_frame(self.world_to_local, by),
            ..self
        }
    }

    fn metadata(&self) -> ProbeMetadata {
        // Within REACH, as the grid keeps influences, box projection's hit
        // and capture centre stay a finite distance apart
        // (probe_parallax_direction); a proxy that large already acts as
        // distant.
        let reach = Vec3::splat(REACH);
        let proxy = self.proxy.unwrap_or(self.influence);
        let proxy = SpecularProbeBox {
            min: proxy.min.clamp(-reach, reach),
            max: proxy.max.clamp(-reach, reach),
        };
        ProbeMetadata {
            world_to_local: self.world_to_local.to_cols_array_2d(),
            center: self
                .center
                .clamp(proxy.min, proxy.max)
                .extend(f32::from(self.proxy.is_some()))
                .to_array(),
            influence_min: self.influence.min.extend(0.).to_array(),
            influence_max: self.influence.max.extend(0.).to_array(),
            blend: self.blend.extend(0.).to_array(),
            proxy_min: proxy.min.extend(0.).to_array(),
            proxy_max: proxy.max.extend(0.).to_array(),
            sphere: self
                .world_to_local
                .inverse()
                .transform_point3((self.influence.min + self.influence.max) * 0.5)
                .extend((self.influence.max - self.influence.min).length() * 0.5)
                .to_array(),
        }
    }
}

/// The collection's metadata and world grid in one buffer: `placements`'
/// baked probes, which the grid covers, then `live`'s.
fn collection(device: &wgpu::Device, placements: &[Placement], live: &[Live]) -> wgpu::Buffer {
    let influences: Vec<_> = placements
        .iter()
        .map(|placement| (placement.world_to_local, placement.influence))
        .collect();
    let grid = ProbeGrid::new(&influences);
    let mut metadata = CollectionMetadata::new(&grid);
    metadata.counts[0] = (placements.len() + live.len()) as u32;
    metadata.counts[1] = placements.len() as u32;
    let records = placements
        .iter()
        .map(Placement::metadata)
        .chain(live.iter().map(Live::record));
    for (slot, record) in metadata.probes.iter_mut().zip(records) {
        *slot = record;
    }
    // A live probe's record is written in place.
    let usage = if live.is_empty() {
        wgpu::BufferUsages::STORAGE
    } else {
        wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST
    };
    collection_buffer(device, "specular probe collection", &metadata, &grid, usage)
}

pub(crate) fn validate_face_size(face_size: u32, limits: &wgpu::Limits) -> Result<(), ProbeError> {
    if !face_size.is_power_of_two()
        || face_size < 1 << (LEVELS - 1)
        || face_size > limits.max_texture_dimension_2d
    {
        return Err(ProbeError::InvalidDimensions);
    }
    Ok(())
}

impl BakedSpecularProbe {
    fn validate(&self, limits: &wgpu::Limits) -> Result<(), ProbeError> {
        self.validate_placement()?;
        validate_face_size(self.radiance.face_size, limits)?;
        let texels = &self.radiance.texels;
        let expected = payload_size(texels.format(), self.radiance.face_size);
        if texels.bytes().len() != expected {
            return Err(ProbeError::InvalidPayloadLength {
                expected,
                actual: texels.bytes().len(),
            });
        }
        texels.validate_radiance()
    }
}

/// Where a new collection's baked cubes come from.
enum Baked<'a> {
    /// The game's probes, validated and uploaded.
    Upload(&'a [BakedSpecularProbe]),
    /// The collection it replaces, copied on the GPU.
    Keep,
}

impl UploadedProbes {
    pub(crate) fn is_empty(&self) -> bool {
        self.placements.is_empty() && self.live.is_empty()
    }

    /// A collection of `probes` alone.
    #[cfg(all(test, not(target_arch = "wasm32")))]
    pub(crate) fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        probes: &[BakedSpecularProbe],
    ) -> Result<Self, ProbeError> {
        Ok(
            Self::assemble(device, queue, Baked::Upload(probes), Vec::new(), None)?
                .expect("a collection of probes"),
        )
    }

    /// The collection of `baked` probes and the `live` ones, in a new cube
    /// array; none without a probe. The cubes `baked` keeps, and each live
    /// probe's published cube, are copied on the GPU from `previous`, the
    /// collection this one replaces. Every probe shares one face size and
    /// texel encoding, `Rgba16Float` beside live probes, whose cubes the
    /// renderer writes.
    fn assemble(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        baked: Baked<'_>,
        live: Vec<Live>,
        previous: Option<&Self>,
    ) -> Result<Option<Self>, ProbeError> {
        let limits = device.limits();
        let (placements, kept): (Vec<Placement>, _) = match &baked {
            Baked::Upload(probes) => (probes.iter().map(Placement::new).collect(), None),
            Baked::Keep => (
                previous.map_or_else(Vec::new, |previous| previous.placements.clone()),
                previous,
            ),
        };
        let count = placements.len() + live.len();
        if count == 0 {
            return Ok(None);
        }
        if count > MAX_PROBES
            || live.len() > MAX_LIVE_PROBES
            || count as u32 * 6 > limits.max_texture_array_layers
        {
            return Err(ProbeError::TooManyProbes);
        }
        // The baked probes' face size and encoding, which the live ones
        // share; the first live probe's face size without baked probes.
        let of_baked = match &baked {
            Baked::Upload(probes) => probes
                .first()
                .map(|probe| (probe.radiance.face_size, probe.radiance.texels.format())),
            Baked::Keep => kept
                .filter(|kept| !kept.placements.is_empty())
                .map(|kept| (kept.face_size, kept.format)),
        };
        let (face_size, format) = of_baked.unwrap_or_else(|| {
            (
                live.first().map_or(0, |live| live.probe.face_size),
                wgpu::TextureFormat::Rgba16Float,
            )
        });
        if format.required_features() != wgpu::Features::empty()
            && !device.features().contains(format.required_features())
        {
            return Err(ProbeError::CompressionUnsupported);
        }
        if let Baked::Upload(probes) = &baked {
            for probe in *probes {
                probe.validate(&limits)?;
                if probe.radiance.face_size != face_size || probe.radiance.texels.format() != format
                {
                    return Err(ProbeError::MixedRadiance);
                }
            }
        }
        validate_face_size(face_size, &limits)?;
        if !live.is_empty()
            && (format != wgpu::TextureFormat::Rgba16Float
                || live.iter().any(|live| live.probe.face_size != face_size))
        {
            return Err(ProbeError::MixedRadiance);
        }
        // An uncompressed array is a copy source, for the collection that
        // replaces it when a live probe is added or removed.
        let mut usage = wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST;
        if format == wgpu::TextureFormat::Rgba16Float {
            usage |= wgpu::TextureUsages::COPY_SRC;
        }
        let texture = cube_array(
            device,
            "specular probes",
            format,
            face_size,
            count as u32,
            LEVELS,
            usage,
        );
        if let Baked::Upload(probes) = &baked {
            upload(queue, &texture, probes, face_size);
        }
        // What the replaced collection holds that this one keeps.
        let mut copies = Vec::new();
        if let Some(kept) = kept.filter(|kept| !kept.placements.is_empty()) {
            copies.push((0, 0, kept.placements.len() as u32));
        }
        for (index, entry) in live.iter().enumerate() {
            let held = previous.and_then(|previous| {
                let at = previous.live.iter().position(|old| old.id == entry.id)?;
                Some((previous.placements.len() + at) as u32)
            });
            if let (Some(from), Some(_)) = (held, entry.captured) {
                copies.push((from, (placements.len() + index) as u32, 1));
            }
        }
        if let (Some(previous), false) = (previous, copies.is_empty()) {
            let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("specular probe collection"),
            });
            for (from, to, cubes) in copies {
                copy_cubes(
                    &mut encoder,
                    (previous.view.texture(), from),
                    (&texture, to),
                    cubes,
                    face_size,
                );
            }
            queue.submit([encoder.finish()]);
        }
        Ok(Some(Self {
            view: cube_array_view(&texture),
            metadata: collection(device, &placements, &live),
            placements,
            live,
            face_size,
            format,
        }))
    }

    /// Moves the render origin by `by` (`Scene::move_origin`): each probe's
    /// frame, which carries a baked probe's centre, each live probe's
    /// centres, and the world grid built over the baked probes anew. A
    /// capture's radiance is about its centre and is unchanged.
    pub(crate) fn move_origin(&mut self, device: &wgpu::Device, by: Vec3) {
        for placement in &mut self.placements {
            *placement = placement.translated(by);
        }
        for live in &mut self.live {
            live.translate(by);
        }
        self.metadata = collection(device, &self.placements, &self.live);
    }

    pub(crate) fn empty(device: &wgpu::Device) -> Self {
        let grid = ProbeGrid::empty();
        Self {
            view: cube_array_view(&cube_array(
                device,
                "empty specular probe collection",
                wgpu::TextureFormat::Rgba16Float,
                1,
                1,
                1,
                wgpu::TextureUsages::TEXTURE_BINDING,
            )),
            metadata: collection_buffer(
                device,
                "empty specular probe collection",
                &CollectionMetadata::new(&grid),
                &grid,
                wgpu::BufferUsages::STORAGE,
            ),
            placements: Vec::new(),
            live: Vec::new(),
            face_size: 1,
            format: wgpu::TextureFormat::Rgba16Float,
        }
    }
}

/// `metadata`, then `grid`'s words, padded to the struct's 16-byte
/// alignment, which a binding of it must fill.
fn collection_buffer(
    device: &wgpu::Device,
    label: &str,
    metadata: &CollectionMetadata,
    grid: &ProbeGrid,
    usage: wgpu::BufferUsages,
) -> wgpu::Buffer {
    let mut bytes = bytemuck::bytes_of(metadata).to_vec();
    bytes.extend_from_slice(bytemuck::cast_slice(&grid.words));
    bytes.resize(bytes.len().next_multiple_of(16), 0);
    buffer(device, label, &bytes, usage)
}

impl Scene {
    /// Replace the installed baked probes; an empty slice clears them. All
    /// probes must share one face size, the live probes' too, beside which
    /// the baked ones are `Rgba16Float` (`ProbeError::MixedRadiance`
    /// otherwise). Invalid input leaves the previous collection installed.
    /// Receivers take environment specular from the probes whose influence
    /// contains them, then the sky. Live probes keep their published cubes.
    pub fn set_baked_specular_probes(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        probes: &[BakedSpecularProbe],
    ) -> Result<(), super::SceneError> {
        let live = self
            .probes
            .as_ref()
            .map_or_else(Vec::new, |held| held.live.clone());
        self.replace_probes(device, queue, Baked::Upload(probes), live)
    }

    /// Replaces the collection with one of `baked` and `live`, which leaves
    /// it as it was when they are refused.
    fn replace_probes(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        baked: Baked<'_>,
        live: Vec<Live>,
    ) -> Result<(), super::SceneError> {
        self.probes = UploadedProbes::assemble(device, queue, baked, live, self.probes.as_ref())?;
        self.edited();
        // World-space ray hits bind the collection in a renderer's group 0.
        self.resources = super::next_generation();
        Ok(())
    }
}

#[cfg(test)]
pub(crate) fn mirrors() -> [crate::shading::layout_tests::Mirror; 2] {
    use crate::shading::layout_tests::mirror;
    [
        mirror!(
            "geometry",
            "BakedProbe",
            ProbeMetadata,
            [
                world_to_local,
                center,
                influence_min,
                influence_max,
                blend,
                proxy_min,
                proxy_max,
                sphere,
            ]
        ),
        mirror!(
            "geometry",
            "ProbeCollection",
            CollectionMetadata,
            [counts, grid_origin, grid_scale, grid_size, padding, probes]
        ),
    ]
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
