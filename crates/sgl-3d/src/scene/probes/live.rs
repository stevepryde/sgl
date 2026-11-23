//! The scene's live specular probes: each one's placement, its cube in the
//! collection's array and whether that cube holds a published capture, with
//! the `Scene` operations that add, move, read and remove them and what a
//! refresh needs of the scene (the architecture's Scene content, Live
//! specular probe).
use super::{Baked, CollectionMetadata, Placement, ProbeMetadata, translated_frame};
use crate::content::identity::{Identity, LiveSpecularProbeId};
use crate::content::live_specular_probe::LiveSpecularProbe;
use crate::scene::{Scene, SceneError};
use glam::Vec3;

/// One live probe of the collection.
#[derive(Clone, Copy)]
pub(crate) struct Live {
    pub id: LiveSpecularProbeId,
    pub probe: LiveSpecularProbe,
    /// Where its published cube was captured from, in the render frame;
    /// none until its first publication, while it weighs nothing.
    pub captured: Option<Vec3>,
}

impl Live {
    /// Its metadata record: its placement about the centre its published
    /// cube was captured from, which box projection corrects parallax
    /// about, or a probe no receiver is inside before its first
    /// publication.
    pub(super) fn record(&self) -> ProbeMetadata {
        let Some(captured) = self.captured else {
            return ProbeMetadata::nowhere();
        };
        Placement {
            center: self.probe.world_to_local.transform_point3(captured),
            world_to_local: self.probe.world_to_local,
            influence: self.probe.influence,
            blend: self.probe.blend,
            proxy: self.probe.proxy,
        }
        .metadata()
    }

    /// Moves it into a render frame whose origin lies `by` from this one's.
    pub(super) fn translate(&mut self, by: Vec3) {
        self.probe.center -= by;
        self.probe.world_to_local = translated_frame(self.probe.world_to_local, by);
        if let Some(captured) = &mut self.captured {
            *captured -= by;
        }
    }
}

/// A live probe's cube, as a refresh needs it.
pub(crate) struct LiveCube {
    /// The collection's cube array.
    pub texture: wgpu::Texture,
    /// Its cube's index in the array.
    pub cube: u32,
    pub probe: LiveSpecularProbe,
}

impl Scene {
    /// Adds a live specular probe, which `Renderer::refresh_specular_probe`
    /// captures and refreshes on the GPU. It weighs nothing until its first
    /// refresh is published. Every probe of the scene shares one face size,
    /// and baked probes beside live ones are `Rgba16Float`
    /// (`ProbeError::MixedRadiance` otherwise); a scene holds at most 16
    /// live probes (`ProbeError::TooManyProbes`).
    pub fn add_live_specular_probe(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        probe: LiveSpecularProbe,
    ) -> Result<LiveSpecularProbeId, SceneError> {
        probe.validate_placement()?;
        let id = LiveSpecularProbeId::issue(0, crate::scene::next_generation());
        let mut live = self.live_probes().to_vec();
        live.push(Live {
            id,
            probe,
            captured: None,
        });
        self.replace_probes(device, queue, Baked::Keep, live)?;
        Ok(id)
    }

    /// A live probe's placement.
    pub fn live_specular_probe(
        &self,
        id: LiveSpecularProbeId,
    ) -> Result<LiveSpecularProbe, SceneError> {
        self.live_probes()
            .iter()
            .find(|live| live.id == id)
            .map(|live| live.probe)
            .ok_or(SceneError::UnknownLiveSpecularProbe)
    }

    /// Moves a live probe: its influence, blend and proxy apply from the
    /// next frame, to the cube it last published, and its `center` is where
    /// its next refresh captures from. Cheap: one record written in place.
    /// Its `face_size` stays (`ProbeError::MixedRadiance` otherwise): remove
    /// and add it for another.
    pub fn set_live_specular_probe(
        &mut self,
        queue: &wgpu::Queue,
        id: LiveSpecularProbeId,
        probe: LiveSpecularProbe,
    ) -> Result<(), SceneError> {
        let index = self.live_index(id)?;
        probe.validate_placement()?;
        let held = self.probes.as_mut().expect("a live probe's collection");
        if probe.face_size != held.face_size {
            return Err(crate::baked_specular_probe::ProbeError::MixedRadiance.into());
        }
        if held.live[index].probe == probe {
            return Ok(());
        }
        held.live[index].probe = probe;
        self.write_live_record(queue, index);
        Ok(())
    }

    /// Removes a live probe and its cube.
    pub fn remove_live_specular_probe(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        id: LiveSpecularProbeId,
    ) -> Result<(), SceneError> {
        let index = self.live_index(id)?;
        let mut live = self.live_probes().to_vec();
        live.remove(index);
        self.replace_probes(device, queue, Baked::Keep, live)
    }

    /// The live probes, in the order of their cubes.
    fn live_probes(&self) -> &[Live] {
        self.probes.as_ref().map_or(&[], |held| &held.live)
    }

    fn live_index(&self, id: LiveSpecularProbeId) -> Result<usize, SceneError> {
        self.live_probes()
            .iter()
            .position(|live| live.id == id)
            .ok_or(SceneError::UnknownLiveSpecularProbe)
    }

    /// Whether the scene holds a live probe.
    pub(crate) fn has_live_probes(&self) -> bool {
        !self.live_probes().is_empty()
    }

    /// Live probe `id`'s cube and placement; none for a probe the scene
    /// does not hold.
    pub(crate) fn live_probe_cube(&self, id: LiveSpecularProbeId) -> Option<LiveCube> {
        let index = self.live_index(id).ok()?;
        let held = self.probes.as_ref()?;
        Some(LiveCube {
            texture: held.view.texture().clone(),
            cube: (held.placements.len() + index) as u32,
            probe: held.live[index].probe,
        })
    }

    /// Records that live probe `id`'s cube now holds a capture from
    /// `center`, which the caller's submitted commands copied into it: its
    /// record places it about that centre, written through `queue` ahead of
    /// every later submission.
    pub(crate) fn publish_live_probe(
        &mut self,
        queue: &wgpu::Queue,
        id: LiveSpecularProbeId,
        center: Vec3,
    ) {
        let Ok(index) = self.live_index(id) else {
            return;
        };
        let held = self.probes.as_mut().expect("a live probe's collection");
        held.live[index].captured = Some(center);
        self.write_live_record(queue, index);
    }

    /// Writes live probe `index`'s record over the one the collection's
    /// buffer holds.
    fn write_live_record(&mut self, queue: &wgpu::Queue, index: usize) {
        let held = self.probes.as_ref().expect("a live probe's collection");
        let record = held.live[index].record();
        let offset = std::mem::offset_of!(CollectionMetadata, probes)
            + (held.placements.len() + index) * std::mem::size_of::<ProbeMetadata>();
        crate::counters::write_buffer(
            queue,
            &held.metadata,
            offset as u64,
            bytemuck::bytes_of(&record),
        );
        // Ray hits and the dynamic GI volume's hits take probe specular.
        self.edited();
    }
}
