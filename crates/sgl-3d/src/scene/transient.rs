//! Caller-authored transient geometry the transparent stage draws (additive
//! glow, heat shimmer and ground mist) and the fog volumes the fog stage
//! fills, with the buffers that hold them.
use super::SceneError;
use crate::content::transient::{
    FogVolume, Glow, GlowKind, HeatDistortion, MAX_DISPLACEMENT_PIXELS, MAX_VERTICES,
};
use crate::shading::fog::{self, FogVolumeRecord};
use crate::shading::vertex::GlowVertex;
use glam::Vec3;

/// The glow and heat buffers' usage: drawn from, written, and read back by
/// tests, which compare what a render origin move left in them.
const GLOW_USAGE: wgpu::BufferUsages = wgpu::BufferUsages::VERTEX
    .union(wgpu::BufferUsages::COPY_DST)
    .union(wgpu::BufferUsages::COPY_SRC);

pub(crate) struct Transient {
    /// Additive glow vertices; `glow_count` of them are drawn.
    pub glow: wgpu::Buffer,
    pub glow_count: u32,
    /// The glow as the game gave it, which a render origin move translates.
    glow_vertices: Vec<Glow>,
    /// Heat triangle vertices; `heat_count` of them are drawn.
    pub heat: wgpu::Buffer,
    pub heat_count: u32,
    /// The heat triangles as the game gave them.
    heat_vertices: Vec<HeatDistortion>,
    /// Mist positions, back to front from the last sorted eye.
    pub mist_positions: Vec<[f32; 3]>,
    pub mist: wgpu::Buffer,
    /// Fog volume records; the first `fog_volume_corners.len()` are the
    /// scene's.
    pub fog_volumes: wgpu::Buffer,
    /// The world corners of each of the scene's fog volumes, in the records'
    /// order.
    pub fog_volume_corners: Vec<[Vec3; 8]>,
    /// The fog volumes as the game gave them.
    fog_volume_list: Vec<FogVolume>,
}

impl Transient {
    /// No glow, heat or mist.
    pub fn new(device: &wgpu::Device) -> Self {
        Self {
            glow: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("transient glow"),
                size: std::mem::size_of::<GlowVertex>() as u64,
                usage: GLOW_USAGE,
                mapped_at_creation: false,
            }),
            glow_count: 0,
            glow_vertices: Vec::new(),
            heat: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("bounded heat vertices"),
                size: (MAX_VERTICES * std::mem::size_of::<HeatDistortion>()) as u64,
                usage: GLOW_USAGE,
                mapped_at_creation: false,
            }),
            heat_count: 0,
            heat_vertices: Vec::new(),
            mist: mist_buffer(device, 1),
            mist_positions: Vec::new(),
            fog_volumes: fog_volume_buffer(device, 1),
            fog_volume_corners: Vec::new(),
            fog_volume_list: Vec::new(),
        }
    }

    /// Replaces the fog volumes, growing the retained buffer as needed. An
    /// invalid volume fails and keeps the previous ones.
    pub fn update_fog_volumes(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        volumes: &[FogVolume],
    ) -> Result<(), SceneError> {
        let valid = |volume: &FogVolume| {
            let rotation = volume.rotation;
            volume.center.is_finite()
                && rotation.is_finite()
                && rotation.length_squared() > 0.
                && volume.size.is_finite()
                && volume.size.min_element() > 0.
                && [volume.density, volume.edge_fade]
                    .into_iter()
                    .chain(volume.albedo)
                    .all(|value| value.is_finite() && value >= 0.)
        };
        if !volumes.iter().all(valid) {
            return Err(SceneError::InvalidFogVolume);
        }
        self.write_fog_volumes(device, queue, volumes);
        Ok(())
    }

    /// Writes `volumes`' records, growing the retained buffer as needed.
    fn write_fog_volumes(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        volumes: &[FogVolume],
    ) {
        let records: Vec<_> = volumes.iter().map(FogVolumeRecord::new).collect();
        let bytes: &[u8] = bytemuck::cast_slice(&records);
        if bytes.len() as u64 > self.fog_volumes.size() {
            self.fog_volumes = fog_volume_buffer(device, volumes.len() as u64);
        }
        if !bytes.is_empty() {
            queue.write_buffer(&self.fog_volumes, 0, bytes);
        }
        self.fog_volume_corners = volumes.iter().map(fog::corners).collect();
        self.fog_volume_list = volumes.to_vec();
    }

    /// Replaces the mist's positions, growing the retained buffer as needed.
    pub fn update_mist(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        positions: &[[f32; 3]],
    ) {
        let bytes = bytemuck::cast_slice(positions);
        if bytes.len() as u64 > self.mist.size() {
            self.mist = mist_buffer(device, positions.len() as u64);
        }
        if !bytes.is_empty() {
            queue.write_buffer(&self.mist, 0, bytes);
        }
        self.mist_positions = positions.to_vec();
    }

    /// Replaces the glow drawn this frame, growing the retained buffer as needed.
    /// An empty slice clears the draw without discarding the buffer reservation.
    pub fn update_glow(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, vertices: &[Glow]) {
        let records: Vec<_> = vertices.iter().map(GlowVertex::new).collect();
        let bytes: &[u8] = bytemuck::cast_slice(&records);
        if bytes.len() as u64 > self.glow.size() {
            self.glow = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("transient glow"),
                size: (bytes.len() as u64).max(self.glow.size() * 2),
                usage: GLOW_USAGE,
                mapped_at_creation: false,
            });
        }
        self.glow_count = vertices.len() as u32;
        if !bytes.is_empty() {
            queue.write_buffer(&self.glow, 0, bytes);
        }
        self.glow_vertices = vertices.to_vec();
    }

    pub fn update_heat(
        &mut self,
        queue: &wgpu::Queue,
        vertices: &[HeatDistortion],
    ) -> Result<(), SceneError> {
        if vertices.len() > MAX_VERTICES || !vertices.len().is_multiple_of(3) {
            return Err(SceneError::HeatVertexCount);
        }
        if vertices.iter().any(|v| {
            v.position.iter().any(|x| !x.is_finite())
                || v.displacement
                    .iter()
                    .any(|x| !x.is_finite() || x.abs() > MAX_DISPLACEMENT_PIXELS)
                || !v.weight.is_finite()
                || !(0.0..=1.0).contains(&v.weight)
        }) {
            return Err(SceneError::InvalidHeatVertex);
        }
        self.heat_count = vertices.len() as u32;
        if self.heat_count > 0 {
            queue.write_buffer(&self.heat, 0, bytemuck::cast_slice(vertices));
        }
        self.heat_vertices = vertices.to_vec();
        Ok(())
    }

    /// Moves the render origin by `by` (`Scene::move_origin`): the glow,
    /// heat and mist positions and the fog volumes' centres, rewritten as
    /// their updates write them.
    pub fn move_origin(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, by: Vec3) {
        let moved = |position: [f32; 3]| (Vec3::from_array(position) - by).to_array();
        let mut glow = std::mem::take(&mut self.glow_vertices);
        for vertex in &mut glow {
            vertex.position = moved(vertex.position);
            if let GlowKind::Line { other, .. } = &mut vertex.kind {
                *other = moved(*other);
            }
        }
        self.update_glow(device, queue, &glow);
        for vertex in &mut self.heat_vertices {
            vertex.position = moved(vertex.position);
        }
        if !self.heat_vertices.is_empty() {
            queue.write_buffer(&self.heat, 0, bytemuck::cast_slice(&self.heat_vertices));
        }
        for position in &mut self.mist_positions {
            *position = moved(*position);
        }
        let mut volumes = std::mem::take(&mut self.fog_volume_list);
        for volume in &mut volumes {
            volume.center -= by;
        }
        self.write_fog_volumes(device, queue, &volumes);
    }

    /// Orders the mist back to front from `eye`, as Three.js orders
    /// transparent objects, and uploads it.
    pub fn sort_mist(&mut self, queue: &wgpu::Queue, eye: Vec3) {
        self.mist_positions.sort_by(|a, b| {
            Vec3::from_array(*b)
                .distance_squared(eye)
                .total_cmp(&Vec3::from_array(*a).distance_squared(eye))
        });
        if !self.mist_positions.is_empty() {
            queue.write_buffer(&self.mist, 0, bytemuck::cast_slice(&self.mist_positions));
        }
    }
}

fn mist_buffer(device: &wgpu::Device, positions: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("retained mist positions"),
        size: positions * std::mem::size_of::<[f32; 3]>() as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn fog_volume_buffer(device: &wgpu::Device, volumes: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("fog volumes"),
        size: volumes * std::mem::size_of::<FogVolumeRecord>() as u64,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}
