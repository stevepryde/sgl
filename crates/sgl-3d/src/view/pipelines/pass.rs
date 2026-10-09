//! The geometry passes: what each one draws, the colour targets it writes
//! and its depth test and write.
use super::variant::Alpha;
use crate::shading::gbuffer;

/// What a geometry pass writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum GeometryPass {
    /// Lit colour and motion with depth: probe captures and standalone
    /// camera draws.
    Forward,
    /// The G-buffer and depth.
    GBuffer,
    /// Anisotropy over the G-buffer's depth, on devices whose attachment
    /// budget leaves it out of `GBuffer`.
    GBufferAnisotropy,
    /// Lit colour, its ambient light, motion and source identity over the
    /// G-buffer's depth; with `shadow_mask`, the camera's surfaces take the
    /// lights the ray-traced shadow mask holds from it, bound at group 3
    /// (`shading::bind::shadow_mask`).
    Lighting {
        shadow_mask: bool,
    },
    /// `GBuffer` and `Lighting` in one pass.
    Fused,
    /// A GPU-built directional cascade's casters, pulled as the camera's
    /// are, their positions from the positions slab their set binds.
    DirectionalShadow,
    /// A GPU-built directional cascade's paired casters of opaque materials
    /// (`shading::culling::CULL_PAIRED`, `SET_PAIRS`): `DirectionalShadow`'s,
    /// drawn indexed over `PAIRED_INDICES`, each slot pulling its corner.
    PairedShadow,
    /// A probe capture's directional cascades' casters, from a CPU-built
    /// list, indexed from the geometry slabs.
    CaptureShadow,
    /// A local-light shadow face's casters, indexed from the geometry slabs.
    LocalShadow,
    /// Blended surfaces' lit colour over the beauty, premultiplied by their
    /// alpha, tested against the opaque depth without writing it, with the blended group 3
    /// (`shading::bind::blended`); with `fsr2_masks`, also FSR2's reactive
    /// and transparency-and-composition masks (`mask_targets`).
    Blended {
        fsr2_masks: bool,
    },
    /// Blended receivers of screen-space reflections as the surface: their
    /// traced lobe into the receiver layer and their motion into the
    /// G-buffer's, over the surface depth, tested strictly nearer and
    /// written. Draws only the receiver batches of the blended list.
    Receivers,
    /// FSR2's transparency and composition mask, 1 over the opaque
    /// surfaces whose shading moves where their geometry stands still
    /// (`Material::surface_moves`), at the G-buffer's depth, with the
    /// reactive mask kept (`composition_targets`).
    Fsr2Composition,
    /// The volume layers (`stages::transparent::volumes`): the depth of the
    /// nearest faces of the blended materials whose shader reads its volume
    /// path, over a copy of the opaque depth, tested strictly nearer and
    /// written, with no colour and no face culled: their front faces
    /// (`VolumeEntry`), their back faces (`VolumeExit`), and their back
    /// faces behind the exit layer's (`VolumeSecondExit`, which reads that
    /// layer through the blended group 3). Draws only those batches of the
    /// blended list.
    VolumeEntry,
    VolumeExit,
    VolumeSecondExit,
}

impl GeometryPass {
    pub(super) fn caster(self) -> bool {
        matches!(
            self,
            Self::DirectionalShadow | Self::PairedShadow | Self::CaptureShadow | Self::LocalShadow
        )
    }

    /// Whether this pass draws materials whose alpha mode requires `alpha`:
    /// `PairedShadow` opaque ones alone.
    pub(super) fn draws(self, alpha: Alpha) -> bool {
        if self == Self::PairedShadow {
            return alpha == Alpha::Opaque;
        }
        matches!(
            self,
            Self::Blended { .. }
                | Self::Receivers
                | Self::VolumeEntry
                | Self::VolumeExit
                | Self::VolumeSecondExit
        ) == (alpha == Alpha::Blend)
    }

    /// Whether the pass draws a volume layer.
    pub fn volume(self) -> bool {
        matches!(
            self,
            Self::VolumeEntry | Self::VolumeExit | Self::VolumeSecondExit
        )
    }

    /// Whether the pass pulls its vertices instead of reading vertex
    /// buffers: every camera and probe pass, nonindexed, so that the split
    /// and fused forms rasterize one primitive stream (`source_vs`), and a
    /// GPU-built cascade's, whose draw instances are sections, nonindexed
    /// or, paired, indexed over a fixed pattern of slots; the CPU-built
    /// lists' shadow casters draw indexed positions (`CasterVertex`).
    /// Opaque casters take no screen-space derivatives, so a GPU-built cascade draws
    /// them indexed where their triangles pair; a masked material's casters
    /// sample its base map with implicit derivatives
    /// (`material_base_color`), so its GPU-built ones stay pulled.
    ///
    /// No camera or probe pass may draw indexed vertex buffers. On Apple
    /// GPUs (M5, Metal) an indexed geometry pass is not deterministic during
    /// a process's first frames, while the driver still grows the tiler's
    /// parameter buffer and splits passes into partial renders (Rosenzweig,
    /// "The Apple GPU and the impossible bug"): two encodes of the same pass
    /// in one frame differ at primitive-edge quads in every output that
    /// uses screen-space derivatives (the mapped normal, the
    /// variance-filtered roughness), by up to a few per cent, which FSR2's
    /// history then keeps (#354). The same pass over pulled vertices is
    /// bit-exact between encodes and between runs.
    pub fn pulled(self) -> bool {
        !matches!(self, Self::CaptureShadow | Self::LocalShadow)
    }

    /// Whether the pass's casters pull their positions from a positions
    /// slab, which its draws bind as group 3 (`shading::bind::caster_positions`):
    /// a GPU-built cascade's.
    pub fn binds_caster_positions(self) -> bool {
        matches!(self, Self::DirectionalShadow | Self::PairedShadow)
    }
}

/// The depth write and test of each pass. A material/depth prepass and its
/// Equal passes must select the same last draw when distinct materials have
/// indistinguishable device depth. The receiver pass and the volume layers
/// test strictly nearer, so a receiver or a face coplanar with opaque
/// geometry leaves that the surface, and the blended draw nearer or equal.
pub(crate) fn depth(pass: GeometryPass) -> (bool, wgpu::CompareFunction) {
    use wgpu::CompareFunction::*;
    match pass {
        GeometryPass::Forward
        | GeometryPass::DirectionalShadow
        | GeometryPass::PairedShadow
        | GeometryPass::CaptureShadow
        | GeometryPass::LocalShadow
        | GeometryPass::Receivers
        | GeometryPass::VolumeEntry
        | GeometryPass::VolumeExit
        | GeometryPass::VolumeSecondExit => (true, Greater),
        GeometryPass::GBuffer | GeometryPass::Fused => (true, GreaterEqual),
        GeometryPass::GBufferAnisotropy
        | GeometryPass::Lighting { .. }
        | GeometryPass::Fsr2Composition => (false, Equal),
        GeometryPass::Blended { .. } => (false, GreaterEqual),
    }
}

/// The colour targets `pass` writes, `GBuffer` with anisotropy when
/// `anisotropy_inline`.
pub(super) fn targets(pass: GeometryPass, anisotropy_inline: bool) -> Vec<wgpu::TextureFormat> {
    use GeometryPass::*;
    match pass {
        Forward => vec![gbuffer::COLOR, gbuffer::MOTION],
        GBuffer => {
            let mut targets = vec![
                gbuffer::NORMAL,
                gbuffer::MATERIAL,
                gbuffer::MOTION,
                gbuffer::F0,
            ];
            if anisotropy_inline {
                targets.push(gbuffer::ANISOTROPY);
            }
            targets
        }
        GBufferAnisotropy => vec![gbuffer::ANISOTROPY],
        Lighting { .. } => vec![
            gbuffer::COLOR,
            gbuffer::AMBIENT,
            gbuffer::MOTION,
            gbuffer::SOURCE_ID,
        ],
        Fused => vec![
            gbuffer::NORMAL,
            gbuffer::MATERIAL,
            gbuffer::MOTION,
            gbuffer::F0,
            gbuffer::COLOR,
            gbuffer::AMBIENT,
            gbuffer::SOURCE_ID,
            gbuffer::ANISOTROPY,
        ],
        DirectionalShadow | PairedShadow | CaptureShadow | LocalShadow => Vec::new(),
        Blended { .. } => vec![gbuffer::COLOR],
        Receivers => vec![gbuffer::RECEIVER, gbuffer::MOTION],
        Fsr2Composition | VolumeEntry | VolumeExit | VolumeSecondExit => Vec::new(),
    }
}

/// Whether a device with `limits` takes a pass writing `formats`: their count,
/// and their bytes per sample as WebGPU counts them, each format's byte cost
/// after aligning the running total to its component alignment
/// (https://gpuweb.github.io/gpuweb/#abstract-opdef-calculating-color-attachment-bytes-per-sample,
/// wgpu-core's `validate_color_attachment_bytes_per_sample`). That charges
/// Rgba8Unorm eight bytes although it stores four.
pub(super) fn attachments_fit(limits: &wgpu::Limits, formats: &[wgpu::TextureFormat]) -> bool {
    let bytes = formats.iter().fold(0, |total: u32, format| {
        total.next_multiple_of(format.target_component_alignment().unwrap())
            + format.target_pixel_byte_cost().unwrap()
    });
    formats.len() <= limits.max_color_attachments as usize
        && bytes <= limits.max_color_attachment_bytes_per_sample
}
