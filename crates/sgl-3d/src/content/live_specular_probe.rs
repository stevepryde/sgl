//! A live specular probe's description and its refresh's pace and progress
//! (the architecture's Scene content, Live specular probe, and its Frame,
//! "Live probe refresh").
use super::baked_specular_probe::{BakedSpecularProbe, ProbeError, SpecularProbeBox};
use glam::{Mat4, Vec3};

/// Live probes in one scene at most: a receiver without culled tiles (a
/// probe capture, a ray hit) tests each of them after its grid cell's baked
/// probes (probe_grid.wgsl), so the cap bounds that walk (AR-12), and each
/// holds a cube of its own. Sixteen is one for each of a scene's few
/// reflective movers and rooms in view, with room to spare.
pub(crate) const MAX_LIVE_PROBES: usize = 16;

/// A specular probe the renderer captures and refreshes on the GPU during
/// play (`Scene::add_live_specular_probe`,
/// `Renderer::refresh_specular_probe`): a [`BakedSpecularProbe`]'s placement
/// without its radiance, which stays on the GPU. Receivers use it as they
/// use a baked probe, blended with the baked probes and the other live ones
/// it overlaps. Until its first refresh is published it weighs nothing, and
/// receivers take the other probes and the sky.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LiveSpecularProbe {
    /// Where the next refresh captures from.
    pub center: Vec3,
    /// Rigid world-to-local transform of the influence and proxy boxes.
    pub world_to_local: Mat4,
    /// Receivers inside use this probe, weighted as a baked probe's
    /// (`BakedSpecularProbe::influence`).
    pub influence: SpecularProbeBox,
    /// The distance inside each face of the influence, per local axis,
    /// over which its weight rises from zero to one; zero is a hard edge.
    pub blend: Vec3,
    /// The geometry the capture sees, for box-projected parallax; `None`
    /// treats it as distant, as for a probe that follows a vehicle outdoors.
    pub proxy: Option<SpecularProbeBox>,
    /// Each face's side in texels: a power of two from 64, which every
    /// probe of the scene, baked or live, shares.
    pub face_size: u32,
    /// The visibility groups its refresh leaves out of its faces: a
    /// material with any of these bits in its `visibility_group` is not
    /// drawn into the probe, and still casts its shadows there. Give the
    /// reflecting object's material a group and name it here, so the object
    /// is not in its own probe (Godot's `ReflectionProbe.cull_mask`). Zero
    /// leaves nothing out.
    pub hidden_groups: u32,
}

impl LiveSpecularProbe {
    /// Its position, transform, influence, blend and proxy, by a baked
    /// probe's rules.
    pub(crate) fn validate_placement(&self) -> Result<(), ProbeError> {
        BakedSpecularProbe::validate_parts(
            self.center,
            self.world_to_local,
            self.influence,
            self.blend,
            self.proxy,
        )
    }
}

/// How much of a live probe's refresh one call of
/// `Renderer::refresh_specular_probe` does, which bounds what a frame pays
/// for it. A refresh is six faces, then the filter, then publication; the
/// probe shows its last published cube until the new one is whole.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ProbeRefreshPace {
    /// The six faces, the filter and publication in one call: Godot's
    /// `UPDATE_ALWAYS`, three.js's `CubeCamera.update`.
    Whole,
    /// The six faces in one call, the filter and publication in the next:
    /// Unity's "all faces at once".
    FacesThenFilter,
    /// One face a call, then the filter and publication in a seventh:
    /// Unity's "individual faces". The lowest cost a call, and the most
    /// calls before the probe changes.
    #[default]
    FacePerCall,
}

/// What a call of `Renderer::refresh_specular_probe` left.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ProbeRefresh {
    /// The refresh advanced and is not finished: the probe still shows its
    /// last published cube. Call again.
    InFlight,
    /// The call published the new cube: frames rendered from now on sample
    /// it.
    Published,
    /// Another probe's refresh is in flight in this renderer, which holds
    /// one at a time: nothing was done. Call again once that one is
    /// published.
    Waiting,
}
