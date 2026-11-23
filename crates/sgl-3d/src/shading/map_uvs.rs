//! A material's map coordinates (the architecture's Material map
//! coordinates): which UV set each of group 2's map bindings samples and
//! the transform it samples it through. This module owns the matrix and the
//! record's words; `map_uv.wgsl`, its twin, applies them.
use super::Module;
use super::bind::group2::MAP_BINDINGS;
use crate::asset::{MaterialTexture, UvSet, UvTransform};

/// The transform's WGSL twin, with raster's pipeline constants.
pub(crate) static MAP_UV: Module = Module {
    name: "map_uv",
    source: include_str!("map_uv.wgsl"),
    deps: &[],
};

/// The shift of `map_uv_bits`' transformed half (`MAP_UV_TRANSFORMED_SHIFT`).
const TRANSFORMED_SHIFT: u32 = 16;

/// One map binding's transform as the shaders read it (map_uv.wgsl's
/// `MapUv`): the rows of KHR_texture_transform's matrix, translation times
/// rotation times scale, so `u' = u[0] * u + u[1] * v + u[2]`.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct MapUv {
    pub u: [f32; 3],
    _pad0: f32,
    pub v: [f32; 3],
    _pad1: f32,
}

impl MapUv {
    /// `transform`'s matrix: `u' = Sx cos r u + Sy sin r v + Ox` and
    /// `v' = -Sx sin r u + Sy cos r v + Oy`, as the Khronos glTF Sample
    /// Renderer ff40317 (source/gltf/material.js 118-160: translation *
    /// rotation * scale, the rotation's columns (c, -s) and (s, c)) and
    /// three.js r187 (examples/jsm/loaders/GLTFLoader.js 2129-2146)
    /// compute it. The extension's prose (the coordinates turn
    /// counter-clockwise about an upper-left origin, V down) and its worked
    /// example require this turn; its GLSL listing read as column-major
    /// gives the other.
    pub fn of(transform: &UvTransform) -> Self {
        let (sin, cos) = f64::from(transform.rotation).sin_cos();
        let [sx, sy] = transform.scale.map(f64::from);
        Self {
            u: [(sx * cos) as f32, (sy * sin) as f32, transform.offset[0]],
            _pad0: 0.,
            v: [(-sx * sin) as f32, (sy * cos) as f32, transform.offset[1]],
            _pad1: 0.,
        }
    }

    /// `uv` through it, as `map_uv_transform` applies it.
    #[cfg(test)]
    pub fn apply(&self, [u, v]: [f32; 2]) -> [f32; 2] {
        [
            self.u[0] * u + self.u[1] * v + self.u[2],
            self.v[0] * u + self.v[1] * v + self.v[2],
        ]
    }
}

/// Where a material's maps in effect sample: each map binding's UV set and
/// transform, in `MAP_BINDINGS` order. A binding without a map samples the
/// first set untransformed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct MapUvs {
    uv1: u16,
    transformed: u16,
    rows: [MapUv; MAP_BINDINGS.len()],
}

impl MapUvs {
    /// Of the map each binding holds, in `MAP_BINDINGS` order, none where a
    /// binding holds no map in effect.
    pub fn new(maps: [Option<MaterialTexture>; MAP_BINDINGS.len()]) -> Self {
        let bits = |set: &dyn Fn(&MaterialTexture) -> bool| {
            maps.iter()
                .enumerate()
                .filter(|(_, map)| map.as_ref().is_some_and(set))
                .fold(0, |bits, (index, _)| bits | 1 << index)
        };
        Self {
            uv1: bits(&|map| map.uv_set == UvSet::Uv1),
            transformed: bits(&|map| map.transform != UvTransform::IDENTITY),
            rows: maps
                .map(|map| MapUv::of(&map.map_or(UvTransform::IDENTITY, |map| map.transform))),
        }
    }

    /// A bit, by `MAP_BINDINGS` index, for each map binding that samples
    /// the second UV set (`Variant::uv1`).
    pub fn uv1(&self) -> u16 {
        self.uv1
    }

    /// A bit, by `MAP_BINDINGS` index, for each map binding whose transform
    /// is not the identity (`Variant::transformed`).
    pub fn transformed(&self) -> u16 {
        self.transformed
    }

    /// The record's `map_uv_bits`: `uv1` in its low half and `transformed`
    /// in its high half.
    pub fn bits(&self) -> u32 {
        u32::from(self.uv1) | u32::from(self.transformed) << TRANSFORMED_SHIFT
    }

    /// Each binding's transform, the identity for one without.
    pub fn rows(&self) -> &[MapUv; MAP_BINDINGS.len()] {
        &self.rows
    }
}

impl Default for MapUvs {
    /// Every binding on the first set, untransformed.
    fn default() -> Self {
        Self::new([None; MAP_BINDINGS.len()])
    }
}

/// Group 2's second uniform (bind_material.wgsl's `MaterialBindings`): what
/// the scene keeps of a material beside the values an edit rewrites.
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct MaterialBindings {
    /// Whether lightmap charts light it, in its first word.
    pub baked: [u32; 4],
    pub map_uvs: [MapUv; MAP_BINDINGS.len()],
}

/// The WGSL twins of the encoding.
#[cfg(test)]
pub(crate) fn constants() -> Vec<crate::shading::layout_tests::Constant> {
    use crate::shading::layout_tests::Constant;
    [
        ("MAP_UV_TRANSFORMED_SHIFT", TRANSFORMED_SHIFT),
        ("MATERIAL_SLOTS", MAP_BINDINGS.len() as u32),
    ]
    .map(|(name, value)| Constant::new("geometry", name, naga::Literal::U32(value)))
    .into()
}

/// `MapUv` and `MaterialBindings`.
#[cfg(test)]
pub(crate) fn mirrors() -> [crate::shading::layout_tests::Mirror; 2] {
    use crate::shading::layout_tests::mirror;
    [
        mirror!("geometry", "MapUv", MapUv, [u, v]),
        mirror!(
            "geometry",
            "MaterialBindings",
            MaterialBindings,
            [baked, map_uvs]
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn close(a: [f32; 2], b: [f32; 2]) -> bool {
        (a[0] - b[0]).abs() < 1e-6 && (a[1] - b[1]).abs() < 1e-6
    }

    // KHR_texture_transform's first worked example (KhronosGroup/glTF
    // 08e10b3, README): offset (0, 1), rotation pi/2 and scale 0.5 use
    // "only the lower left quadrant of the source image, rotated clockwise
    // 90 degrees". With the origin at the image's upper left and V down,
    // that quadrant is U in 0..0.5 and V in 0.5..1, and an image turned
    // clockwise shows its left edge along the surface's top: the surface's
    // corners (0,0), (1,0), (0,1), (1,1) read the image's lower left corner
    // (0,1), the quadrant's upper left (0,0.5), the image's bottom centre
    // (0.5,1) and its centre (0.5,0.5).
    #[wasm_bindgen_test(unsupported = test)]
    fn the_extensions_rotated_quadrant_example_reads_the_lower_left_quadrant() {
        let m = MapUv::of(&UvTransform {
            offset: [0., 1.],
            rotation: std::f32::consts::FRAC_PI_2,
            scale: [0.5, 0.5],
        });
        for (uv, image) in [
            ([0., 0.], [0., 1.]),
            ([1., 0.], [0., 0.5]),
            ([0., 1.], [0.5, 1.]),
            ([1., 1.], [0.5, 0.5]),
        ] {
            assert!(close(m.apply(uv), image), "{uv:?} read {:?}", m.apply(uv));
        }
    }

    // Its second example: offset (0, 1) and scale (1, -1) invert the T
    // axis, "effectively defining a bottom-left origin": v' = 1 - v.
    #[wasm_bindgen_test(unsupported = test)]
    fn the_extensions_inverted_t_example_flips_v() {
        let m = MapUv::of(&UvTransform {
            offset: [0., 1.],
            rotation: 0.,
            scale: [1., -1.],
        });
        assert!(close(m.apply([0.25, 0.]), [0.25, 1.]));
        assert!(close(m.apply([0.75, 0.75]), [0.75, 0.25]));
    }

    // Scale applies before rotation and offset last (the extension's
    // translation * rotation * scale): a point on the U axis scaled by 2,
    // turned a quarter turn (counter-clockwise with V down: +U to -V) and
    // moved by (10, 20) lands at (10, 20 - 2).
    #[wasm_bindgen_test(unsupported = test)]
    fn scale_then_rotation_then_offset() {
        let m = MapUv::of(&UvTransform {
            offset: [10., 20.],
            rotation: std::f32::consts::FRAC_PI_2,
            scale: [2., 3.],
        });
        assert!(close(m.apply([1., 0.]), [10., 18.]));
        // And a point on the V axis, scaled by 3, turns onto +U.
        assert!(close(m.apply([0., 1.]), [13., 20.]));
    }

    fn map(uv_set: UvSet, transform: UvTransform) -> Option<MaterialTexture> {
        Some(MaterialTexture {
            uv_set,
            transform,
            ..MaterialTexture::new(0)
        })
    }

    // Each binding's bits are its own: the second set in the low half, a
    // transform in the high half, by the binding's index.
    #[wasm_bindgen_test(unsupported = test)]
    fn bits_name_each_binding_alone() {
        let mut maps = [None; MAP_BINDINGS.len()];
        let scaled = UvTransform {
            scale: [2., 2.],
            ..UvTransform::IDENTITY
        };
        maps[0] = map(UvSet::Uv0, UvTransform::IDENTITY);
        maps[2] = map(UvSet::Uv1, UvTransform::IDENTITY);
        maps[3] = map(UvSet::Uv0, scaled);
        maps[5] = map(UvSet::Uv1, scaled);
        let uvs = MapUvs::new(maps);
        assert_eq!(uvs.uv1(), 0b10_0100);
        assert_eq!(uvs.transformed(), 0b10_1000);
        assert_eq!(uvs.bits(), 0b10_0100 | 0b10_1000 << 16);
        assert_eq!(uvs.rows()[3].u, [2., 0., 0.]);
        // An untransformed binding's rows are the identity's: u' = u, v' = v.
        assert_eq!(
            (uvs.rows()[0].u, uvs.rows()[0].v),
            ([1., 0., 0.], [0., 1., 0.])
        );
        assert_eq!(MapUvs::default().bits(), 0);
    }
}
