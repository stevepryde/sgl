//! Shadows: the directional light's shadow cascades and the local-light
//! shadow atlas, drawn from draw lists of the views they prepare.
//!
//! Reads: the cascade views and their draw lists (`FrameViews`), the scene's
//! lights and instances, group 0's shadow groups, the geometry pipelines.
//! Writes: its own depth maps and the lights' shadow records, which
//! `FrameBindings` lends to lit group 0 through [`ShadowMaps`].
//! Honours: the frame's directional shadow cascades, the effective local
//! lights and the shadow quality's map sizes.
//! Timing groups: `directional shadow cascade 0` to `3`, `local shadow
//! layers`, `local shadows`.
pub(crate) mod directional;
pub(crate) mod local;

use crate::settings::ShadowQuality;
use crate::view::bindings::ShadowMaps;
use crate::view::frame::FrameContext;

pub(crate) struct Shadows {
    pub directional: directional::Directional,
    pub local: local::Local,
    sampler: wgpu::Sampler,
}

impl Shadows {
    /// Maps of `quality`'s sizes.
    pub fn new(device: &wgpu::Device, quality: ShadowQuality) -> Self {
        Self {
            directional: directional::Directional::new(device, quality.cascade_size()),
            local: local::Local::new(device, quality.atlas_size()),
            sampler: device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("shadow comparison"),
                compare: Some(wgpu::CompareFunction::GreaterEqual),
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                ..Default::default()
            }),
        }
    }

    /// Reallocates the maps whose size `quality` changes; the local atlas
    /// then places and draws every shadow anew. Cheap when nothing changed.
    pub fn resize(&mut self, device: &wgpu::Device, quality: ShadowQuality) {
        if self.directional.size() != quality.cascade_size() {
            self.directional = directional::Directional::new(device, quality.cascade_size());
        }
        self.local.resize(device, quality.atlas_size());
    }

    /// The maps, records and sampler to lend to group 0.
    pub fn maps(&self) -> ShadowMaps {
        ShadowMaps {
            directional: self.directional.array.clone(),
            local_atlas: self.local.atlas().clone(),
            local_layers: self.local.layers().clone(),
            local_records: self.local.records.clone(),
            sampler: self.sampler.clone(),
        }
    }

    /// The camera's directional shadow cascades, when the frame has them.
    pub fn encode_directional(&self, ctx: &mut FrameContext<'_>) {
        self.directional.encode(ctx);
    }

    /// The local-light shadow faces whose content changed.
    pub fn encode_local(&mut self, ctx: &mut FrameContext<'_>) {
        self.local.encode(ctx);
    }
}
