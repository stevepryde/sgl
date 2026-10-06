//! The scene's ray queries: the WGSL modules that trace the scene, through
//! the portable walk or the hardware path's query of the device's form, and
//! the root a tracing pipeline composes.
use super::{BC7, BIND_SCENE, DEFORMATION, MATERIAL, Module, PACKED_VERTEX, SCENE_SOURCE, SRGB};

/// The scene's geometry and materials for ray queries, at group 1, and the
/// object records a hit reads its instance's pose, flags and ambient cube
/// from.
pub(crate) static SCENE_RAYS: Module = Module {
    name: "scene_rays",
    source: include_str!("scene_rays.wgsl"),
    deps: &[
        &MATERIAL,
        &SCENE_SOURCE,
        &SRGB,
        &PACKED_VERTEX,
        &BIND_SCENE,
        &BC7,
        &DEFORMATION,
    ],
};
/// The one acceptance predicate of every scene ray, the ray's validity test
/// and the side policies (`SCENE_SIDES_*`), which every trace composes.
pub(crate) static SCENE_RAYS_PREDICATE: Module = Module {
    name: "scene_rays_predicate",
    source: include_str!("scene_rays_predicate.wgsl"),
    deps: &[&SCENE_RAYS],
};
/// The portable walk: the BVH traversal over the source header's roots,
/// which the portable function set and the hardware module both compose.
pub(crate) static SCENE_RAYS_WALK: Module = Module {
    name: "scene_rays_walk",
    source: include_str!("scene_rays_walk.wgsl"),
    deps: &[&SCENE_RAYS_PREDICATE],
};
/// The scene ray function set through the portable walk alone: a tracing
/// pipeline's root where the hardware path is not in effect
/// (`ray_trace_root`).
pub(crate) static SCENE_RAYS_PORTABLE: Module = Module {
    name: "scene_rays_portable",
    source: include_str!("scene_rays_portable.wgsl"),
    deps: &[&SCENE_RAYS_WALK],
};
/// The hardware path's shared module: the TLAS binding, the re-trace, the
/// composition with the portable walk, the per-ray budget and the scene ray
/// function set. It calls the query of the form in effect, whose module is
/// the root that composes it, never a dependency of it: `compose` recurses
/// forever on a cycle.
pub(crate) static SCENE_RAYS_HARDWARE: Module = Module {
    name: "scene_rays_hardware",
    source: include_str!("scene_rays_hardware.wgsl"),
    deps: &[&SCENE_RAYS_WALK],
};
/// The baseline form's query (`RayQueryForm::Baseline`), the root of a
/// hardware-traced pipeline on every native backend.
pub(crate) static SCENE_RAYS_QUERY_OPAQUE: Module = Module {
    name: "scene_rays_query_opaque",
    source: include_str!("scene_rays_query_opaque.wgsl"),
    deps: &[&SCENE_RAYS_HARDWARE],
};
/// The candidate form's query (`RayQueryForm::Candidates`), the root of a
/// hardware-traced pipeline on a backend that may run the candidate form
/// (`RayQueryForm::lowered`: Vulkan and DX12).
pub(crate) static SCENE_RAYS_QUERY_CANDIDATES: Module = Module {
    name: "scene_rays_query_candidates",
    source: include_str!("scene_rays_query_candidates.wgsl"),
    deps: &[&SCENE_RAYS_HARDWARE],
};

/// The form of the hardware path's ray queries (the architecture's Hardware
/// ray tracing, *Two forms, one stage*), a capability of the device's
/// backend: it selects the query module a tracing pipeline composes and the
/// geometry flags the scene builds its BLASes with; nothing else branches
/// on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum RayQueryForm {
    /// Every BLAS geometry opaque, one query per look and a re-trace past
    /// a committed hit the shared predicate rejects; masked models'
    /// instances are traced by the portable walk.
    Baseline,
    /// A masked mesh's geometry not opaque, its instances in the TLAS, and
    /// a candidate loop that runs the shared predicate on each of its
    /// triangles a ray crosses, confirming those it accepts.
    Candidates,
}

/// The form a device whose backend may run the candidate form runs
/// (`RayQueryForm::lowered`: Vulkan and DX12): the baseline until the
/// candidate form's benefit is measured on their hardware (RD-6), a
/// recorded decision and never a setting (AR-3). The candidate form has not
/// run on Vulkan or DX12 hardware yet (#23): the owner has none that traces
/// rays, and MoltenVK offers no ray query. Metal's naga 30 lowers the loop
/// too, but Metal stays on the baseline by recorded decision until #211.
pub(crate) const LOWERED_FORM: RayQueryForm = RayQueryForm::Baseline;

impl RayQueryForm {
    /// Whether `backend` may run a candidate loop: Vulkan and DX12, whose
    /// naga 30 SPIR-V and HLSL writers lower it. naga 30's MSL writer lowers
    /// it too, through Metal's `intersection_query` (`back/msl/ray.rs`
    /// 363–495), but Metal keeps the baseline, a recorded decision, until
    /// the candidate form is validated and measured there (#211). No other
    /// backend has ray queries.
    pub fn lowered(backend: wgpu::Backend) -> bool {
        matches!(backend, wgpu::Backend::Vulkan | wgpu::Backend::Dx12)
    }

    /// The form a device of `backend` runs: `lowered`, the form chosen for
    /// backends that may run the candidate form (`LOWERED_FORM`), where
    /// `backend` is one, else the baseline.
    pub fn of_backend(backend: wgpu::Backend, lowered: Self) -> Self {
        if Self::lowered(backend) {
            lowered
        } else {
            Self::Baseline
        }
    }
}

/// The root a tracing pipeline composes after its own modules: the hardware
/// path's query module for the form in effect, else the portable function
/// set. Each defines the scene ray function set once in the program.
pub(crate) fn ray_trace_root(form: Option<RayQueryForm>) -> &'static Module {
    match form {
        None => &SCENE_RAYS_PORTABLE,
        Some(RayQueryForm::Baseline) => &SCENE_RAYS_QUERY_OPAQUE,
        Some(RayQueryForm::Candidates) => &SCENE_RAYS_QUERY_CANDIDATES,
    }
}

#[cfg(test)]
mod form_tests {
    use super::RayQueryForm;
    use wasm_bindgen_test::wasm_bindgen_test;

    // Plausible defect: a backend that cannot run a candidate loop given
    // the candidate form once the recorded choice for Vulkan and DX12
    // (`LOWERED_FORM`) becomes it. The oracle is what the architecture
    // records: naga 30's SPIR-V and HLSL writers lower the loop, Metal
    // keeps the baseline until #211 validates the candidate form there,
    // and no other backend has ray queries.
    #[wasm_bindgen_test(unsupported = test)]
    fn only_vulkan_and_dx12_take_the_candidate_form() {
        use wgpu::Backend::{BrowserWebGpu, Dx12, Gl, Metal, Noop, Vulkan};
        for (backend, lowered) in [
            (Vulkan, true),
            (Dx12, true),
            (Metal, false),
            (Gl, false),
            (BrowserWebGpu, false),
            (Noop, false),
        ] {
            let form = RayQueryForm::of_backend(backend, RayQueryForm::Candidates);
            let expected = if lowered {
                RayQueryForm::Candidates
            } else {
                RayQueryForm::Baseline
            };
            assert_eq!(form, expected, "{backend:?}");
            assert_eq!(
                RayQueryForm::of_backend(backend, RayQueryForm::Baseline),
                RayQueryForm::Baseline,
                "{backend:?}"
            );
        }
    }
}
