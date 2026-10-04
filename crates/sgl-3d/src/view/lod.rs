//! The camera's choice among a mesh's registered alternatives
//! (`Scene::set_mesh_lods`), bounded in its pixels.
use crate::lod::MeshLod;
use crate::scene::models::{Mesh, Models};
use glam::{Mat4, Vec3};

/// Selects alternatives for one camera: its view, projection and pixel size.
#[derive(Clone, Copy)]
pub(crate) struct LodSelector {
    view: Mat4,
    projection: Mat4,
    size: [u32; 2],
}

impl LodSelector {
    /// None for a camera without pixels, which keeps full detail.
    pub fn new(view: Mat4, projection: Mat4, size: [u32; 2]) -> Option<Self> {
        (!size.contains(&0)).then_some(Self {
            view,
            projection,
            size,
        })
    }

    /// The last admissible alternative of `mesh` at `pose`, bounded to half a
    /// pixel, or None for the mesh itself.
    pub fn select<'a>(&self, mesh: &'a Mesh, models: &Models, pose: Mat4) -> Option<&'a MeshLod> {
        let bounds = mesh.ranges.bounds()?;
        let transforms = [pose, self.view, self.projection];
        mesh.lods.iter().rev().find(|lod| {
            let alternative = &models
                .slots
                .get(lod.model)
                .expect("a level of detail's model lives")
                .meshes[lod.mesh];
            let bounds = alternative.ranges.bounds().map_or(bounds, |other| {
                [bounds[0].min(other[0]), bounds[1].max(other[1])]
            });
            projected_error(transforms, bounds, lod.max_error, self.size) <= 0.5
        })
    }
}

// Bound perspective division directly, including lateral motion and changing W.
// For |delta p| <= e, |delta clip_i| <= e * |row_i.xyz|. The quotient
// difference is bounded by (delta_i + |ndc_i| delta_w)/(w - delta_w).
// Compose in f64, but also bound the shader's separate f32 model/view/projection
// operations. Exact CPU composition alone misses cancellation after a rounded
// world-space translation (e.g. centimetre detail near a million metres).
fn projected_error(transforms: [Mat4; 3], bounds: [Vec3; 2], error: f32, size: [u32; 2]) -> f64 {
    if !transforms.iter().all(|m| m.is_finite()) || !bounds.iter().all(|v| v.is_finite()) {
        return f64::INFINITY;
    }
    let mut matrix = glam::DMat4::IDENTITY;
    let mut magnitude = glam::DMat4::IDENTITY;
    for transform in transforms {
        let transform = transform.as_dmat4();
        matrix = transform * matrix;
        magnitude =
            glam::DMat4::from_cols_array(&transform.to_cols_array().map(f64::abs)) * magnitude;
    }
    // Three four-component dot products along each dependency chain use at
    // most 21 roundings without FMA. 32*f32::EPSILON conservatively exceeds
    // gamma_21, including the few f64 operations constructing this bound.
    let largest = bounds[0].abs().max(bounds[1].abs()).as_dvec3().extend(1.);
    let rounding = magnitude * largest * (32. * f64::from(f32::EPSILON));
    let rows = matrix.transpose();
    let mut delta = [rows.x_axis, rows.y_axis, rows.z_axis, rows.w_axis]
        .map(|row| row.truncate().length() * f64::from(error));
    // The original and alternative can round in opposite directions.
    for (delta, round) in delta.iter_mut().zip(rounding.to_array()) {
        *delta += 2. * round;
    }
    let mut minimum_w = f64::INFINITY;
    let mut maximum_ndc = glam::DVec2::ZERO;
    for x in [bounds[0].x, bounds[1].x] {
        for y in [bounds[0].y, bounds[1].y] {
            for z in [bounds[0].z, bounds[1].z] {
                let clip = matrix * Vec3::new(x, y, z).as_dvec3().extend(1.);
                // Reversed-Z: the near plane is z = w.
                let near = clip.w - clip.z - rounding.w - rounding.z;
                if clip.w - rounding.w <= delta[3] || near <= delta[3] + delta[2] {
                    return f64::INFINITY;
                }
                minimum_w = minimum_w.min(clip.w - rounding.w);
                maximum_ndc = maximum_ndc.max(
                    (glam::DVec2::new(clip.x, clip.y).abs() + rounding.truncate().truncate())
                        / (clip.w - rounding.w),
                );
            }
        }
    }
    let pixels = (glam::DVec2::new(delta[0], delta[1]) + maximum_ndc * delta[3])
        / (minimum_w - delta[3])
        * glam::DVec2::new(size[0] as f64, size[1] as f64)
        * 0.5;
    pixels.length()
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;
    #[wasm_bindgen_test(unsupported = test)]
    fn error_bound_contains_independently_projected_displacements() {
        let projection = crate::perspective(1.1, 1.5, 0.1);
        for pose in [Mat4::IDENTITY, Mat4::from_scale(Vec3::new(-3., 0.3, 2.))] {
            let matrix = projection * pose;
            let bounds = [Vec3::new(-4., -2., -90.), Vec3::new(7., 3., -80.)];
            let bound =
                projected_error([pose, Mat4::IDENTITY, projection], bounds, 0.02, [900, 600]);
            for p in [bounds[0], bounds[1], (bounds[0] + bounds[1]) * 0.5] {
                for direction in [
                    Vec3::X,
                    Vec3::Y,
                    Vec3::Z,
                    Vec3::ONE.normalize(),
                    -Vec3::ONE.normalize(),
                ] {
                    let a = matrix.as_dmat4() * p.as_dvec3().extend(1.);
                    let b =
                        matrix.as_dmat4() * (p.as_dvec3() + direction.as_dvec3() * 0.02).extend(1.);
                    let observed = ((a.truncate() / a.w - b.truncate() / b.w).truncate()
                        * glam::DVec2::new(450., 300.))
                    .length();
                    assert!(observed <= bound, "{observed} > {bound}");
                }
            }
        }
        // A 1cm displacement at 100m on a 90-degree, 1000px camera is 0.05px.
        let matrix = crate::perspective(std::f32::consts::FRAC_PI_2, 1., 0.1);
        let bound = projected_error(
            [Mat4::IDENTITY, Mat4::IDENTITY, matrix],
            [Vec3::new(0., 0., -100.); 2],
            0.01,
            [1000, 1000],
        );
        assert!((0.05..0.072).contains(&bound));
    }
    #[wasm_bindgen_test(unsupported = test)]
    fn near_plane_crossing_never_simplifies() {
        let p = crate::perspective(1., 1., 0.1);
        for z in [0.1, 0., -0.05, -0.10001] {
            assert!(
                projected_error(
                    [Mat4::IDENTITY, Mat4::IDENTITY, p],
                    [Vec3::new(0., 0., z); 2],
                    0.01,
                    [1000; 2]
                )
                .is_infinite()
            );
        }
        assert!(
            projected_error(
                [Mat4::IDENTITY, Mat4::IDENTITY, p],
                [Vec3::new(0., 0., -100.); 2],
                0.01,
                [1000; 2]
            ) < 0.5
        );
    }
    #[wasm_bindgen_test(unsupported = test)]
    fn staged_f32_translation_cannot_admit_visible_lod_displacement() {
        let projection = crate::perspective(std::f32::consts::FRAC_PI_2, 1., 0.1);
        let original = Vec3::new(0.02, 0., -100.);
        let alternative = Vec3::new(0.04, 0., -100.);
        for translation in [0., 1e6, -1e6] {
            let pose = Mat4::from_translation(Vec3::new(translation, 0., 0.));
            let view = Mat4::from_translation(Vec3::new(-translation, 0., 0.));
            // Exercise the shader's staged f32 arithmetic, not the analytical
            // bound or a precomposed transform. Large translation rounds these
            // two local positions to world positions separated by 0.0625m.
            let raster = |p: Vec3| {
                let world = pose * p.extend(1.);
                let camera = view * world;
                let clip = projection * camera;
                glam::Vec2::new(clip.x, clip.y) / clip.w * 960.
            };
            let observed = (raster(alternative) - raster(original)).length() as f64;
            let bound = projected_error(
                [pose, view, projection],
                [original, alternative],
                0.02,
                [1920; 2],
            );
            assert!(
                bound >= observed,
                "bound {bound} missed staged GPU displacement {observed}"
            );
            if translation == 0. {
                assert!(bound < 0.5, "ordinary local-space LOD must remain useful");
            } else {
                assert!(
                    observed > 0.5,
                    "regression fixture must expose visible rounding"
                );
                assert!(
                    bound > 0.5,
                    "uncertain large-world LOD must retain full detail"
                );
            }
        }
    }
}
