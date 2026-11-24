//! Writing Three.js r185's PMREM atlas (CubeUV) on the GPU: where each level
//! and face lies (the writer's half of the layout `probe_sampling.wgsl`'s
//! `pmrem_bilinear` reads), the incremental GGX roughness each level is
//! filtered with, and `pmrem_filter.wgsl`, the texel directions and the GGX
//! filter, ported from three.js 2431a09 `src/extras/PMREMGenerator.js`.

/// The texel directions and the GGX filter.
pub(crate) static PMREM_FILTER: super::Module = super::Module {
    name: "pmrem_filter",
    source: sgl_post_fx::embed_wgsl!("pmrem_filter.wgsl"),
    deps: &[&super::PROBE_SAMPLING],
};

/// Three's `LOD_MIN`: the smallest face, 2^4 texels a side, which the extra,
/// blurrier levels share.
const LOD_MIN: u32 = 4;
/// Three's `EXTRA_LOD_SIGMA` count: the levels past the mip chain.
const EXTRA_LEVELS: u32 = 6;

/// One level of an atlas: its face size and the texel at its face 0's
/// corner. Faces 0 to 2 lie in its first row and 3 to 5 in its second.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AtlasLevel {
    pub size: u32,
    pub origin: [u32; 2],
}

impl AtlasLevel {
    /// The texel at face `face`'s corner.
    pub fn face_origin(&self, face: u32) -> [u32; 2] {
        [
            self.origin[0] + face % 3 * self.size,
            self.origin[1] + face / 3 * self.size,
        ]
    }
}

/// The CubeUV atlas of a cube `cube` texels a side (a power of two, at
/// least 16): its levels, sharpest first, as Three's `_createPlanes` and
/// `_applyGGXFilter` place them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Atlas {
    pub cube: u32,
}

impl Atlas {
    /// Three's `_lodMax`.
    fn lod_max(self) -> u32 {
        self.cube.ilog2()
    }

    /// Its levels: the mip chain down to `LOD_MIN`, then the extra levels.
    pub fn levels(self) -> u32 {
        self.lod_max() - LOD_MIN + 1 + EXTRA_LEVELS
    }

    /// Its width and height in texels (Three's `_allocateTarget`).
    pub fn size(self) -> [u32; 2] {
        [3 * self.cube.max(16 * 7), 4 * self.cube]
    }

    /// Level `level` (0 sharpest): Three's `_sizeLods` and viewport,
    /// `x = 3 size (level - lodMax + LOD_MIN)` for the extra levels and
    /// `y = 4 (cube - size)`.
    pub fn level(self, level: u32) -> AtlasLevel {
        let lod = self.lod_max().saturating_sub(level).max(LOD_MIN);
        let size = 1 << lod;
        let extra = (level + LOD_MIN).saturating_sub(self.lod_max());
        AtlasLevel {
            size,
            origin: [3 * size * extra, 4 * (self.cube - size)],
        }
    }

    /// The roughness `_applyGGXFilter` filters level `level` (1 or more)
    /// from the level before with: the incremental roughness from the
    /// source's to the target's (`level / (levels - 1)`), times Three's
    /// blur-strength mapping, 1.25 times the target's.
    pub fn filter_roughness(self, level: u32) -> f32 {
        let last = (self.levels() - 1) as f32;
        let target = level as f32 / last;
        let source = (level - 1) as f32 / last;
        let incremental = (target * target - source * source).sqrt();
        incremental * (target * 1.25)
    }

    /// The mip `pmrem_bilinear` reads level `level` at (Three's `mipInt`,
    /// `lodMax - level`).
    pub fn mip(self, level: u32) -> f32 {
        self.lod_max() as f32 - level as f32
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
