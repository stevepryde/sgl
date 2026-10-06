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
/// hardware-traced pipeline on Vulkan and DX12 (`LOWERED_FORM`) and on a
/// device that fell back from the candidate form.
pub(crate) static SCENE_RAYS_QUERY_OPAQUE: Module = Module {
    name: "scene_rays_query_opaque",
    source: include_str!("scene_rays_query_opaque.wgsl"),
    deps: &[&SCENE_RAYS_HARDWARE],
};
/// The candidate form's query (`RayQueryForm::Candidates`), the root of a
/// hardware-traced pipeline on a backend that runs the candidate form
/// (`RayQueryForm::of_backend`: Metal).
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

/// The form Vulkan and DX12 run (`RayQueryForm::of_backend`): the baseline
/// until the candidate form's benefit is measured on their hardware (RD-6),
/// a recorded decision and never a setting (AR-3). The candidate form has
/// not run on Vulkan or DX12 hardware yet (#23): the owner has none that
/// traces rays, and MoltenVK offers no ray query.
pub(crate) const LOWERED_FORM: RayQueryForm = RayQueryForm::Baseline;

/// The form Metal runs (`RayQueryForm::of_backend`): the candidate form, a
/// recorded decision and never a setting (AR-3), the owner's choice on
/// these measurements on an Apple M5 (#211; release builds, median GPU ms
/// of the passes that trace, candidates against the baseline). Where
/// masked content is traced, the candidate loop replaces the portable
/// walk, or a deforming instance's re-traces: among 266 static hedges of
/// cut-out cards at 1920×1080 the frame took 19.2 against 22.6 (dynamic
/// GI rays 0.31 against 0.79, ray-traced shadow rays 1.11 against 2.15,
/// world reflection rays 3.37 against 5.23), and among a crowd's masked
/// hair cards at 960×540 dynamic GI rays took 0.25 against 0.36. Where
/// nothing is masked it costs its larger program: 2–3 % of the streaming
/// example's ray-traced shadow rays on its walk and fly (about 0.02),
/// nothing measurable on the dynamic GI example or the world reflection
/// rays, and 14 % of the ray-traced shadow rays over a thousand opaque
/// props under nine shadowed lights (1.00 against 0.88, 1.2 % of the
/// frame), of which forcing opacity recovers a quarter, so most is the
/// loop's code, not its traversal.
pub(crate) const METAL_FORM: RayQueryForm = RayQueryForm::Candidates;

impl RayQueryForm {
    /// Whether `backend` may run a candidate loop: Vulkan, DX12 and Metal,
    /// whose naga 30 SPIR-V, HLSL and MSL writers lower it (the MSL writer
    /// through Metal's `intersection_query`, `back/msl/ray.rs` 363–495,
    /// validated on an Apple M5 by #211). No other backend has ray queries.
    pub fn lowered(backend: wgpu::Backend) -> bool {
        matches!(
            backend,
            wgpu::Backend::Vulkan | wgpu::Backend::Dx12 | wgpu::Backend::Metal
        )
    }

    /// The form a device of `backend` runs: the form recorded for it
    /// (`METAL_FORM`, else `LOWERED_FORM`) where it may run the candidate
    /// form, else the baseline.
    pub fn of_backend(backend: wgpu::Backend) -> Self {
        let recorded = match backend {
            wgpu::Backend::Metal => METAL_FORM,
            _ => LOWERED_FORM,
        };
        Self::gated(backend, recorded)
    }

    /// `form` where `backend` may run the candidate form, else the
    /// baseline.
    fn gated(backend: wgpu::Backend, form: Self) -> Self {
        if Self::lowered(backend) {
            form
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
    // the candidate form once a recorded choice (`LOWERED_FORM`,
    // `METAL_FORM`) becomes it. The oracle is what the architecture
    // records: naga 30's SPIR-V, HLSL and MSL writers lower the loop, #211
    // validated the MSL lowering on an Apple M5, and no other backend has
    // ray queries.
    #[wasm_bindgen_test(unsupported = test)]
    fn only_backends_that_lower_the_loop_take_the_candidate_form() {
        use wgpu::Backend::{BrowserWebGpu, Dx12, Gl, Metal, Noop, Vulkan};
        for (backend, lowered) in [
            (Vulkan, true),
            (Dx12, true),
            (Metal, true),
            (Gl, false),
            (BrowserWebGpu, false),
            (Noop, false),
        ] {
            let form = RayQueryForm::gated(backend, RayQueryForm::Candidates);
            let expected = if lowered {
                RayQueryForm::Candidates
            } else {
                RayQueryForm::Baseline
            };
            assert_eq!(form, expected, "{backend:?}");
            assert_eq!(
                RayQueryForm::gated(backend, RayQueryForm::Baseline),
                RayQueryForm::Baseline,
                "{backend:?}"
            );
        }
    }
}
