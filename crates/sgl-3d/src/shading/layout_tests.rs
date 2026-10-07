//! Every composed program parses and validates and holds exactly the entry
//! points its pipelines are created with, every Rust mirror of a WGSL
//! struct has its members' names, offsets and size as naga lays them out, and
//! every vertex buffer layout supplies what its vertex entry points read.
use super::compose;
use wasm_bindgen_test::wasm_bindgen_test;

/// A Rust struct mirroring the WGSL struct `wgsl` of program `program`, with
/// its field offsets in WGSL member order.
pub(crate) struct Mirror {
    pub program: &'static str,
    pub wgsl: &'static str,
    pub rust: &'static str,
    pub size: usize,
    pub fields: Vec<(&'static str, usize)>,
}

/// `mirror!("program", "WgslStruct", RustType, [field, rust_field as "wgsl_member", ...])`.
macro_rules! mirror {
    ($program:literal, $wgsl:literal, $rust:ty, [$($field:ident $(as $member:literal)?),* $(,)?]) => {
        $crate::shading::layout_tests::Mirror {
            program: $program,
            wgsl: $wgsl,
            rust: stringify!($rust),
            size: std::mem::size_of::<$rust>(),
            fields: vec![$((
                $crate::shading::layout_tests::mirror!(@member $field $($member)?),
                std::mem::offset_of!($rust, $field),
            )),*],
        }
    };
    (@member $field:ident $member:literal) => {
        $member
    };
    (@member $field:ident) => {
        stringify!($field)
    };
}
pub(crate) use mirror;

/// Every program the crate composes, by its root module's name.
fn programs() -> Vec<(&'static str, String)> {
    let roots: [&'static super::Module; 31] = [
        &crate::shading::PACKED_VERTEX,
        &crate::view::pipelines::CASTER,
        &crate::stages::opaque::sky::SKY,
        &crate::stages::transparent::effects::GLOW,
        &crate::stages::transparent::mist::MIST,
        &crate::stages::fog::VOLUMETRIC_FOG,
        &crate::stages::transparent::heat::HEAT,
        &crate::stages::opaque::ambient_occlusion::XE_GTAO,
        &crate::view::post_fx::INPUTS,
        &crate::stages::reflections::source::COMPLETION,
        &crate::stages::reflections::probe_culling::PROBE_CULLING,
        &crate::stages::reflections::world::DENOISE,
        &crate::stages::reflections::world::classify::CLASSIFY,
        &crate::stages::post::smaa::SMAA,
        &crate::stages::probe_prefilter::PREFILTER,
        &crate::stages::post::bloom::BLOOM,
        &crate::stages::post::tone_map::TONE_MAP,
        &crate::stages::exposure::EXPOSURE,
        &crate::stages::deform::DEFORM,
        &crate::stages::cull::CULL,
        &crate::stages::cull::pyramid::PYRAMID,
        &crate::stages::dynamic_gi::pipelines::ALLOCATE,
        &crate::stages::dynamic_gi::pipelines::UPDATE,
        &crate::stages::motion_blur::MOTION_BLUR,
        &crate::stages::shadows::traced::TEMPORAL,
        &crate::stages::shadows::traced::UPSAMPLE,
        &crate::stages::shadows::traced::denoise::TILE_CLASSIFICATION_FOUR,
        &crate::stages::shadows::traced::denoise::TILE_CLASSIFICATION_ONE,
        &crate::stages::shadows::traced::denoise::FILTER_FOUR,
        &crate::stages::shadows::traced::denoise::FILTER_ONE,
        &crate::stages::shadows::local::COPY,
    ];
    let mut programs: Vec<_> = roots
        .into_iter()
        .chain(
            crate::stages::reflections::velvet::PROGRAMS
                .iter()
                .map(|&(root, _)| root),
        )
        .map(|root| (root.name, compose(&[root])))
        .collect();
    // The geometry program with each shadow-mask provider: exactly one,
    // the mask's for the lighting pass while ray-traced shadows run.
    programs.extend([
        ("geometry", crate::view::pipelines::geometry_program(false)),
        (
            "geometry_shadow_mask",
            crate::view::pipelines::geometry_program(true),
        ),
    ]);
    // The tracing stages' programs on each path their rays take, with the
    // portable function set or a hardware form's query module as root
    // (`ray_trace_root`); the hardware module's `enable` directive must
    // reach the head.
    programs.extend(
        traced_programs(None)
            .into_iter()
            .chain(hardware_programs())
            .map(|(label, source, _)| (label, source)),
    );
    programs.push(("lit_compute_library", crate::shading::lit_compute_library()));
    #[cfg(feature = "diagnostics")]
    programs.extend([
        (
            "frame_probe",
            compose(&[&crate::stages::frame_probe::PROBE]),
        ),
        (
            "frame_probe_coverage",
            compose(&[&crate::stages::frame_probe::COVERAGE]),
        ),
        (
            "dynamic_gi_observe",
            compose(&[&crate::stages::dynamic_gi::observe::OBSERVE]),
        ),
    ]);
    programs
}

/// A program's label, source and entry points.
type Program = (&'static str, String, Vec<(naga::ShaderStage, &'static str)>);

/// The entry points each program's pipelines are created with, by the
/// program's label: the constants beside its root that the pipeline code
/// names. Both geometry programs hold the geometry passes' entries, and a
/// tracing stage's program on each path its rays take holds its root's. A
/// library validated whole is listed nowhere and holds none.
fn pipeline_entries() -> Vec<(&'static str, Vec<&'static str>)> {
    use super::FULLSCREEN_VS_ENTRY;
    use crate::stages::cull::{self, pyramid};
    use crate::stages::dynamic_gi::pipelines as gi;
    use crate::stages::opaque::{ambient_occlusion as ao, sky};
    use crate::stages::post::{bloom, inputs, smaa, tone_map};
    use crate::stages::reflections::{probe_culling, source, velvet, world};
    use crate::stages::shadows::{local, traced, traced::denoise};
    use crate::stages::transparent::{effects, heat, mist};
    use crate::stages::{deform, exposure, fog, motion_blur, probe_prefilter};
    use crate::view::{pipelines as view, post_fx};
    let geometry = vec![
        view::SOURCE_VS_ENTRY,
        view::FS_ENTRY,
        view::STABLE_FS_ENTRY,
        view::STABLE_LEGACY_FS_ENTRY,
        view::ANISOTROPY_FS_ENTRY,
        view::SOURCE_FS_ENTRY,
        view::FUSED_OPAQUE_FS_ENTRY,
        view::BLENDED_FS_ENTRY,
        view::BLENDED_FSR2_MASKED_FS_ENTRY,
        view::RECEIVER_FS_ENTRY,
        view::FSR2_COMPOSITION_FS_ENTRY,
    ];
    let mut entries = vec![
        (view::GEOMETRY.name, geometry.clone()),
        ("geometry_shadow_mask", geometry),
        (
            view::CASTER.name,
            vec![
                view::SHADOW_VS_ENTRY,
                view::SHADOW_UNCLIPPED_VS_ENTRY,
                view::SHADOW_MASKED_VS_ENTRY,
                view::SHADOW_MASKED_UNCLIPPED_VS_ENTRY,
                view::SHADOW_PULLED_VS_ENTRY,
                view::SHADOW_PULLED_UNCLIPPED_VS_ENTRY,
                view::SHADOW_PULLED_MASKED_VS_ENTRY,
                view::SHADOW_PULLED_MASKED_UNCLIPPED_VS_ENTRY,
                view::SHADOW_PAIRED_VS_ENTRY,
                view::SHADOW_PAIRED_UNCLIPPED_VS_ENTRY,
                view::SHADOW_UNCLIPPED_FS_ENTRY,
                view::SHADOW_MASKED_FS_ENTRY,
                view::SHADOW_MASKED_UNCLIPPED_FS_ENTRY,
            ],
        ),
        (sky::SKY.name, vec![sky::SKY_VS_ENTRY, sky::SKY_FS_ENTRY]),
        (
            effects::GLOW.name,
            vec![
                effects::GLOW_VS_ENTRY,
                effects::GLOW_SOFT_FS_ENTRY,
                effects::GLOW_SOFT_FSR2_MASKED_FS_ENTRY,
            ],
        ),
        (
            mist::MIST.name,
            vec![
                mist::MIST_VS_ENTRY,
                mist::MIST_FS_ENTRY,
                mist::MIST_FSR2_MASKED_FS_ENTRY,
            ],
        ),
        (
            fog::VOLUMETRIC_FOG.name,
            vec![
                fog::INJECT_ENTRY,
                fog::FILTER_FROXELS_ENTRY,
                fog::INTEGRATE_ENTRY,
            ],
        ),
        (heat::HEAT.name, vec![heat::VS_ENTRY, heat::FS_ENTRY]),
        (
            ao::XE_GTAO.name,
            vec![
                ao::PREFILTER_ENTRY,
                ao::PREFILTER_MIP4_ENTRY,
                ao::MAIN_PASS_ENTRY,
                ao::DENOISE_ENTRY,
            ],
        ),
        (
            post_fx::INPUTS.name,
            vec![FULLSCREEN_VS_ENTRY, post_fx::FS_MAIN_ENTRY],
        ),
        (
            source::COMPLETION.name,
            vec![
                source::MAIN_ENTRY,
                FULLSCREEN_VS_ENTRY,
                source::COMPOSE_SCREEN_SPACE_ENTRY,
            ],
        ),
        (
            probe_culling::PROBE_CULLING.name,
            vec![probe_culling::MAIN_ENTRY],
        ),
        (
            world::DENOISE.name,
            vec![
                world::WORLD_RESOLVE_ENTRY,
                world::WORLD_TEMPORAL_ENTRY,
                world::WORLD_UPSAMPLE_ENTRY,
            ],
        ),
        (
            world::classify::CLASSIFY.name,
            vec![
                world::classify::CLASSIFY_ENTRY,
                world::classify::PREPARE_RAYS_ENTRY,
            ],
        ),
        (
            smaa::SMAA.name,
            vec![
                smaa::DETECT_VERTEX_ENTRY,
                smaa::DETECT_ENTRY,
                smaa::CALCULATE_VERTEX_ENTRY,
                smaa::CALCULATE_ENTRY,
                smaa::BLEND_VERTEX_ENTRY,
                smaa::BLEND_ENTRY,
            ],
        ),
        (
            probe_prefilter::PREFILTER.name,
            vec![
                probe_prefilter::PREFILTER_ENTRY,
                probe_prefilter::MIP_REDUCE_ENTRY,
            ],
        ),
        (
            bloom::BLOOM.name,
            vec![
                inputs::VS_ENTRY,
                bloom::DOWNSAMPLE_FIRST_ENTRY,
                bloom::DOWNSAMPLE_ENTRY,
                bloom::UPSAMPLE_ENTRY,
                bloom::COMPOSITE_ENTRY,
            ],
        ),
        (
            tone_map::TONE_MAP.name,
            vec![
                inputs::VS_ENTRY,
                tone_map::PRESENT_ENTRY,
                tone_map::PRESENT_DIRECT_ENTRY,
                tone_map::COPY_PIXEL_ENTRY,
            ],
        ),
        (
            exposure::EXPOSURE.name,
            vec![
                exposure::COMPUTE_HISTOGRAM_ENTRY,
                exposure::COMPUTE_AVERAGE_ENTRY,
            ],
        ),
        (deform::DEFORM.name, vec![deform::DEFORM_ENTRY]),
        (
            cull::CULL.name,
            vec![
                cull::CULL_INSTANCES_ENTRY,
                cull::CULL_FINALIZE_ENTRY,
                cull::CULL_SECTIONS_ENTRY,
                cull::CULL_INSTANCES_LATE_ENTRY,
                cull::CULL_FINALIZE_LATE_ENTRY,
                cull::CULL_SECTIONS_LATE_ENTRY,
            ],
        ),
        (
            pyramid::PYRAMID.name,
            vec![
                pyramid::DOWNSAMPLE_DEPTH_FIRST_ENTRY,
                pyramid::DOWNSAMPLE_DEPTH_SECOND_ENTRY,
            ],
        ),
        (
            gi::ALLOCATE.name,
            vec![
                gi::RANK_ENTRY,
                gi::THRESHOLD_ENTRY,
                gi::ALLOCATE_ENTRY,
                gi::PREPARE_TRACE_ENTRY,
            ],
        ),
        (
            gi::UPDATE.name,
            vec![
                gi::UPDATE_IRRADIANCE_ENTRY,
                gi::UPDATE_DEPTH_ENTRY,
                gi::SETTLE_ENTRY,
                gi::SCROLL_ENTRY,
            ],
        ),
        (
            motion_blur::MOTION_BLUR.name,
            motion_blur::ENTRY_POINTS.to_vec(),
        ),
        (traced::TEMPORAL.name, vec![traced::TEMPORAL_ENTRY]),
        (traced::UPSAMPLE.name, vec![traced::UPSAMPLE_ENTRY]),
        (
            denoise::TILE_CLASSIFICATION_FOUR.name,
            vec![denoise::TILE_CLASSIFICATION_ENTRY],
        ),
        (
            denoise::TILE_CLASSIFICATION_ONE.name,
            vec![denoise::TILE_CLASSIFICATION_ENTRY],
        ),
        (denoise::FILTER_FOUR.name, vec![denoise::FILTER_ENTRY]),
        (denoise::FILTER_ONE.name, vec![denoise::FILTER_ENTRY]),
        (
            local::COPY.name,
            vec![FULLSCREEN_VS_ENTRY, local::COPY_FS_ENTRY],
        ),
    ];
    entries.extend(
        velvet::PROGRAMS
            .iter()
            .map(|&(root, names)| (root.name, names.to_vec())),
    );
    // The dynamic GI stage's observed trace (feature `diagnostics`) is
    // created from its portable program alone, but every path's holds it.
    for (label, _, points) in traced_programs(None).into_iter().chain(hardware_programs()) {
        let mut names: Vec<_> = points.into_iter().map(|(_, name)| name).collect();
        if label.starts_with(gi::TRACE.name) {
            names.push(gi::TRACE_OBSERVED_ENTRY);
        }
        entries.push((label, names));
    }
    #[cfg(feature = "diagnostics")]
    {
        use crate::stages::{dynamic_gi::observe, frame_probe};
        entries.extend([
            ("frame_probe", vec![frame_probe::MAIN_ENTRY]),
            ("frame_probe_coverage", vec![frame_probe::MAIN_ENTRY]),
            ("dynamic_gi_observe", vec![observe::OBSERVE_ENTRY]),
        ]);
    }
    entries
}

/// The tracing stages' programs, and the tests' scene ray dispatch, on the
/// path `form` takes (`ray_trace_root`), each with the pipeline constants
/// it needs and its entry points.
fn traced_programs(form: Option<super::RayQueryForm>) -> Vec<Program> {
    use naga::ShaderStage::Compute;
    let root = super::ray_trace_root(form);
    let world = &crate::stages::reflections::world::TRACE;
    let gi = &crate::stages::dynamic_gi::pipelines::TRACE;
    let query = &crate::scene::rays::QUERY;
    let traced_shadows = &crate::stages::shadows::traced::TRACE;
    let label = |portable, baseline, candidates| match form {
        None => portable,
        Some(super::RayQueryForm::Baseline) => baseline,
        Some(super::RayQueryForm::Candidates) => candidates,
    };
    vec![
        (
            label(
                world.name,
                "world_reflections_hardware",
                "world_reflections_candidates",
            ),
            compose(&[world, root]),
            vec![(
                Compute,
                crate::stages::reflections::world::WORLD_TRACE_ENTRY,
            )],
        ),
        (
            label(
                gi.name,
                "dynamic_gi_trace_hardware",
                "dynamic_gi_trace_candidates",
            ),
            compose(&[gi, root]),
            vec![(Compute, crate::stages::dynamic_gi::pipelines::TRACE_ENTRY)],
        ),
        (
            label(
                traced_shadows.name,
                "traced_shadows_trace_hardware",
                "traced_shadows_trace_candidates",
            ),
            compose(&[traced_shadows, root]),
            vec![(Compute, crate::stages::shadows::traced::TRACE_ENTRY)],
        ),
        (
            label(
                "scene_rays_portable_query",
                "scene_rays_hardware_query",
                "scene_rays_candidates_query",
            ),
            compose(&[query, root]),
            vec![(Compute, crate::scene::rays::SCENE_INTERSECT_ENTRY)],
        ),
    ]
}

/// Every hardware form's traced programs.
fn hardware_programs() -> Vec<Program> {
    [
        super::RayQueryForm::Baseline,
        super::RayQueryForm::Candidates,
    ]
    .into_iter()
    .flat_map(|form| traced_programs(Some(form)))
    .collect()
}

fn parse(label: &str, source: &str) -> naga::Module {
    naga::front::wgsl::parse_str(source)
        .unwrap_or_else(|error| panic!("{label}: {}", error.emit_to_string(source)))
}

// A module missing from a program's dependencies (an unresolved name), a
// declaration made twice, or a type error fails here, without a GPU.
#[wasm_bindgen_test(unsupported = test)]
fn every_program_composes_and_validates() {
    let entries = pipeline_entries();
    for (label, source) in programs() {
        let module = parse(label, &source);
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap_or_else(|error| panic!("{label}: {}", error.emit_to_string(&source)));
        // A module that swallows the code after it, as a block comment left
        // open does (naga reads it to the program's end), leaves a valid
        // program without the entry points its pipelines are created with;
        // an entry point a program holds that no pipeline names is unlisted.
        let mut held: Vec<_> = module
            .entry_points
            .iter()
            .map(|point| point.name.as_str())
            .collect();
        let mut named: Vec<_> = entries
            .iter()
            .filter(|(program, _)| *program == label)
            .flat_map(|(_, names)| names.iter().copied())
            .collect();
        held.sort_unstable();
        named.sort_unstable();
        assert_eq!(
            held, named,
            "{label} holds other entry points than its pipelines are created with"
        );
    }
}

// The hardware path's programs of both forms through naga 30's SPIR-V and
// HLSL writers, as wgpu-hal's Vulkan and DX12 backends write each entry
// point when they create its pipeline (`vulkan/adapter.rs` 2773–2877 sets
// the SPIR-V options followed here; DX12 needs shader model 6.5 for ray
// queries). Plausible defects: a construct that validates but that either
// writer cannot emit in the candidate loop or the baseline query, such as a
// hit's vertex positions read from the query, which naga's HLSL writer
// does not lower (`back/hlsl/writer.rs` 4643–4647): on that backend every
// tracing pipeline of the form would fail, the candidate form falling back
// to the baseline and the baseline leaving the device without a hardware
// trace, where no GPU here can show it. The oracle is the writers the
// backends run; DXC's compile of the HLSL and the driver's of the SPIR-V
// are not run here.
#[wasm_bindgen_test(unsupported = test)]
fn hardware_programs_write_for_vulkan_and_dx12() {
    let spv = naga::back::spv::Options {
        lang_version: (1, 5),
        capabilities: None,
        force_loop_bounding: true,
        ray_query_initialization_tracking: true,
        ..Default::default()
    };
    let hlsl = naga::back::hlsl::Options {
        shader_model: naga::back::hlsl::ShaderModel::V6_5,
        ..Default::default()
    };
    for (label, source, entry_points) in hardware_programs() {
        let module = parse(label, &source);
        let info = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap_or_else(|error| panic!("{label}: {}", error.emit_to_string(&source)));
        // The world-space trace's reach, which its pipelines set.
        let mut constants = naga::back::PipelineConstants::default();
        if module
            .overrides
            .iter()
            .any(|(_, value)| value.name.as_deref() == Some("world_reach_all"))
        {
            constants.insert("world_reach_all".into(), 1.);
        }
        for (stage, name) in entry_points {
            let (module, info) = naga::back::pipeline_constants::process_overrides(
                &module,
                &info,
                Some((stage, name)),
                &constants,
            )
            .unwrap_or_else(|error| panic!("{label} {name}: {error}"));
            naga::back::spv::write_vec(
                &module,
                &info,
                &spv,
                Some(&naga::back::spv::PipelineOptions {
                    shader_stage: stage,
                    entry_point: name.into(),
                }),
            )
            .unwrap_or_else(|error| panic!("{label} {name} as SPIR-V: {error}"));
            let mut written = String::new();
            naga::back::hlsl::Writer::new(
                &mut written,
                &hlsl,
                &naga::back::hlsl::PipelineOptions {
                    entry_point: Some((stage, name.into())),
                },
            )
            .write(&module, &info, None)
            .unwrap_or_else(|error| panic!("{label} {name} as HLSL: {error}"));
        }
    }
}

fn wgsl_constant(module: &naga::Module, name: &str) -> Option<naga::Literal> {
    let (_, constant) = module
        .constants
        .iter()
        .find(|(_, constant)| constant.name.as_deref() == Some(name))?;
    match module.global_expressions[constant.init] {
        naga::Expression::Literal(literal) => Some(literal),
        _ => None,
    }
}

// A field reordered, retyped, added or padded differently in either language
// fails here: offsets come from Rust's layout and naga's WGSL layout.
#[wasm_bindgen_test(unsupported = test)]
fn rust_mirrors_match_wgsl_layouts() {
    use super::uniforms::*;
    let mirrors = [
        mirror!(
            "geometry",
            "View",
            ViewUniform,
            [
                view,
                projection,
                view_projection,
                inverse_view_projection,
                stable_view_projection,
                previous_view_projection,
                eye,
                mip_bias,
                jitter,
                viewport,
                flags,
            ]
        ),
        mirror!(
            "geometry",
            "Frame",
            FrameUniform,
            [
                directional_lights,
                shadow_cascades,
                hemisphere_sky_color,
                hemisphere_intensity,
                hemisphere_ground_color,
                diffuse_environment_yaw,
                backdrop_color,
                diffuse_environment_intensity,
                mist_thin_color,
                mist_opacity,
                mist_dense_color,
                backdrop_yaw,
                mist_size,
                mist_drift,
                backdrop_brightness,
                fog_inverse_length,
                fog_inverse_detail_spread,
                reflection_yaw,
                reflection_intensity,
                elapsed_seconds,
                fixed_irradiance_scale,
                visibility_mask,
                flags,
                shadow_cascade_count,
                frame_count,
                animation_phase,
                lightmap_chart,
                dynamic_gi_origin,
                dynamic_gi_spacing,
                dynamic_gi_probes,
                dynamic_gi_scroll,
                irradiance_volume_origin,
                irradiance_volume_cell_size,
                irradiance_volume_cells,
            ]
        ),
        mirror!(
            "geometry",
            "ShadowCascade",
            ShadowCascadeUniform,
            [clip_from_world, texel_size, far_bound]
        ),
        mirror!(
            "geometry",
            "DirectionalLight",
            DirectionalLightUniform,
            [
                direction_to_light,
                flags,
                color,
                illuminance,
                fog_energy,
                shadow_opacity,
                disc_radius
            ]
        ),
        mirror!(
            "geometry",
            "Object",
            ObjectUniform,
            [
                model,
                previous_model,
                baked_irradiance,
                flags,
                deformed_positions,
                previous_positions,
                deformed_normals
            ]
        ),
        mirror!(
            "geometry",
            "Material",
            super::material::MaterialUniform,
            [
                base,
                emission,
                environment_scale,
                metallic,
                roughness,
                coat,
                coat_roughness,
                normal_scale,
                bump_scale,
                anisotropy_strength,
                anisotropy_rotation,
                alpha_cutoff,
                visibility_group,
                flags,
                occlusion_strength,
                specular_f0,
                specular,
                normal_layers,
            ]
        ),
        mirror!(
            "geometry",
            "NormalLayer",
            super::material::NormalLayerUniform,
            [cycles, scale, strength]
        ),
    ]
    .into_iter()
    .chain(super::bind::mirrors())
    .chain(super::lights::mirrors())
    .chain(super::clusters::mirrors())
    .chain(super::culling::mirrors())
    .chain(super::decals::mirrors())
    .chain(crate::scene::rays::mirrors())
    .chain(crate::shading::deformation::mirrors())
    .chain(crate::scene::probes::mirrors())
    .chain(crate::stages::reflections::source::mirrors())
    .chain(crate::stages::reflections::probe_culling::mirrors())
    .chain(crate::stages::reflections::world::mirrors())
    .chain(crate::stages::reflections::world::classify::mirrors())
    .chain(crate::stages::reflections::velvet::mirrors())
    .chain(crate::stages::opaque::ambient_occlusion::mirrors())
    .chain(crate::stages::exposure::mirrors())
    .chain(crate::stages::motion_blur::mirrors())
    .chain(crate::stages::fog::mirrors())
    .chain(crate::stages::dynamic_gi::mirrors())
    .chain(crate::stages::fog::volume_froxels::mirrors())
    .chain(super::fog::mirrors())
    .chain(crate::stages::post::bloom::mirrors())
    .chain(crate::stages::post::tone_map::mirrors())
    .chain(super::shadow_mask::mirrors())
    .chain(crate::stages::shadows::traced::mirrors());
    let programs = programs();
    let module = |label: &str| {
        let (_, source) = programs
            .iter()
            .find(|(program, _)| *program == label)
            .unwrap_or_else(|| panic!("no program {label}"));
        parse(label, source)
    };
    for mirror in mirrors {
        let program = module(mirror.program);
        let (members, span) = program
            .types
            .iter()
            .find_map(|(_, ty)| match &ty.inner {
                naga::TypeInner::Struct { members, span }
                    if ty.name.as_deref() == Some(mirror.wgsl) =>
                {
                    Some((members, *span))
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("{} declares no struct {}", mirror.program, mirror.wgsl));
        let mut wgsl: Vec<_> = members
            .iter()
            .map(|member| (member.name.as_deref().unwrap_or(""), member.offset as usize))
            .collect();
        // A runtime-sized array that ends the struct (its buffer's tail)
        // starts where the Rust mirror of the rest ends.
        let tail = members.last().filter(|member| {
            matches!(
                program.types[member.ty].inner,
                naga::TypeInner::Array {
                    size: naga::ArraySize::Dynamic,
                    ..
                }
            )
        });
        let size = match tail {
            Some(tail) => {
                wgsl.pop();
                tail.offset
            }
            None => span,
        };
        assert_eq!(
            wgsl, mirror.fields,
            "{} members and offsets differ from {}",
            mirror.wgsl, mirror.rust
        );
        assert_eq!(
            size as usize, mirror.size,
            "{} and {} differ in size",
            mirror.wgsl, mirror.rust
        );
    }
    let uniforms = module("geometry");
    for (name, value) in [
        ("VIEW_PROBE_CAPTURE", VIEW_PROBE_CAPTURE),
        ("DIRECTIONAL_LIGHT_SHADOW", DIRECTIONAL_LIGHT_SHADOW),
        ("FRAME_FOG", FRAME_FOG),
        ("FRAME_BAKED_LIGHTING", FRAME_BAKED_LIGHTING),
        ("FRAME_IRRADIANCE_ATLAS", FRAME_IRRADIANCE_ATLAS),
        ("FRAME_BACKDROP_COLOR", FRAME_BACKDROP_COLOR),
        ("FRAME_TEMPORAL_SHADOW_FILTER", FRAME_TEMPORAL_SHADOW_FILTER),
        ("FRAME_HARDWARE_SHADOW_FILTER", FRAME_HARDWARE_SHADOW_FILTER),
        ("FRAME_DYNAMIC_GI", FRAME_DYNAMIC_GI),
        ("FRAME_IRRADIANCE_VOLUME", FRAME_IRRADIANCE_VOLUME),
        ("MATERIAL_UNLIT", super::material::MATERIAL_UNLIT),
        (
            "MATERIAL_DOUBLE_SIDED",
            super::material::MATERIAL_DOUBLE_SIDED,
        ),
        ("MATERIAL_NORMAL_MAP", super::material::MATERIAL_NORMAL_MAP),
        ("MATERIAL_BUMP_MAP", super::material::MATERIAL_BUMP_MAP),
        (
            "MATERIAL_ANISOTROPY_MAP",
            super::material::MATERIAL_ANISOTROPY_MAP,
        ),
        ("MATERIAL_ALPHA_MASK", super::material::MATERIAL_ALPHA_MASK),
        (
            "MATERIAL_ALPHA_BLEND",
            super::material::MATERIAL_ALPHA_BLEND,
        ),
        (
            "MATERIAL_RECEIVES_SCREEN_SPACE_REFLECTIONS",
            super::material::MATERIAL_RECEIVES_SCREEN_SPACE_REFLECTIONS,
        ),
        (
            "MATERIAL_NORMAL_LAYERS",
            super::material::MATERIAL_NORMAL_LAYERS,
        ),
        (
            "MATERIAL_EMITS_INTO_GI",
            super::material::MATERIAL_EMITS_INTO_GI,
        ),
    ] {
        assert_eq!(
            wgsl_constant(&uniforms, name),
            Some(naga::Literal::U32(value)),
            "{name}"
        );
    }
}

/// The vertex format a shader input of type `inner` reads exactly.
fn vertex_format(inner: &naga::TypeInner) -> wgpu::VertexFormat {
    use naga::{Scalar, TypeInner, VectorSize};
    use wgpu::VertexFormat::*;
    match *inner {
        TypeInner::Scalar(Scalar::U32) => Uint32,
        TypeInner::Scalar(Scalar::F32) => Float32,
        TypeInner::Vector {
            size: VectorSize::Bi,
            scalar: Scalar::F32,
        } => Float32x2,
        TypeInner::Vector {
            size: VectorSize::Tri,
            scalar: Scalar::F32,
        } => Float32x3,
        TypeInner::Vector {
            size: VectorSize::Quad,
            scalar: Scalar::F32,
        } => Float32x4,
        _ => panic!("no vertex format for {inner:?}"),
    }
}

/// Vertex entry point `entry`'s `@location` inputs, from its arguments and
/// their struct members: location, name, format and size in bytes.
fn vertex_inputs(
    module: &naga::Module,
    entry: &str,
) -> Vec<(u32, String, wgpu::VertexFormat, u32)> {
    let entry_point = module
        .entry_points
        .iter()
        .find(|point| point.name == entry && point.stage == naga::ShaderStage::Vertex)
        .unwrap_or_else(|| panic!("no vertex entry point {entry}"));
    let mut inputs = Vec::new();
    let mut input = |binding: &Option<naga::Binding>, name: &Option<String>, ty| {
        if let Some(naga::Binding::Location { location, .. }) = *binding {
            let inner = &module.types[ty].inner;
            inputs.push((
                location,
                name.clone().unwrap_or_default(),
                vertex_format(inner),
                inner.size(module.to_ctx()),
            ));
        }
    };
    for argument in &entry_point.function.arguments {
        match &module.types[argument.ty].inner {
            naga::TypeInner::Struct { members, .. } if argument.binding.is_none() => {
                for member in members {
                    input(&member.binding, &member.name, member.ty);
                }
            }
            _ => input(&argument.binding, &argument.name, argument.ty),
        }
    }
    inputs
}

// An attribute relocated, retyped, renamed or swapped with another on one
// side only, an attribute no shader reads, or a shader input no attribute
// supplies fails here: the inputs come from naga's parse of each entry point
// that reads the buffers, which its pipelines bind together. A layout may
// declare fewer attributes than its record has fields (the scene vertex's
// other fields reach the camera passes through the scene source), so the
// stride is not the inputs' sum.
#[wasm_bindgen_test(unsupported = test)]
fn vertex_layouts_match_wgsl_inputs() {
    use super::vertex::{CASTER_LAYOUT, DRAW_INSTANCE_LAYOUT};
    use crate::stages::transparent::{effects, heat, mist};
    use crate::view::pipelines as view;
    let caster = view::CASTER.name;
    let pipelines = [
        (
            &[&DRAW_INSTANCE_LAYOUT, &CASTER_LAYOUT][..],
            &[
                (caster, view::SHADOW_VS_ENTRY),
                (caster, view::SHADOW_UNCLIPPED_VS_ENTRY),
                (caster, view::SHADOW_MASKED_VS_ENTRY),
                (caster, view::SHADOW_MASKED_UNCLIPPED_VS_ENTRY),
            ][..],
        ),
        (
            &[&DRAW_INSTANCE_LAYOUT][..],
            &[
                (view::GEOMETRY.name, view::SOURCE_VS_ENTRY),
                (caster, view::SHADOW_PULLED_VS_ENTRY),
                (caster, view::SHADOW_PULLED_UNCLIPPED_VS_ENTRY),
                (caster, view::SHADOW_PULLED_MASKED_VS_ENTRY),
                (caster, view::SHADOW_PULLED_MASKED_UNCLIPPED_VS_ENTRY),
            ][..],
        ),
        (
            &[&effects::GLOW_LAYOUT][..],
            &[(effects::GLOW.name, effects::GLOW_VS_ENTRY)][..],
        ),
        (
            &[&heat::HEAT_LAYOUT][..],
            &[(heat::HEAT.name, heat::VS_ENTRY)][..],
        ),
        (
            &[&mist::MIST_LAYOUT][..],
            &[(mist::MIST.name, mist::MIST_VS_ENTRY)][..],
        ),
    ];
    let programs = programs();
    for (layouts, readers) in pipelines {
        let mut read = layouts
            .iter()
            .map(|layout| vec![false; layout.fields.len()])
            .collect::<Vec<_>>();
        for (label, entry) in readers {
            let (_, source) = programs
                .iter()
                .find(|(program, _)| program == label)
                .unwrap_or_else(|| panic!("no program {label}"));
            for (location, name, format, _) in vertex_inputs(&parse(label, source), entry) {
                let (layout, index) = layouts
                    .iter()
                    .enumerate()
                    .find_map(|(layout, buffer)| {
                        let index = buffer
                            .buffer
                            .attributes
                            .iter()
                            .position(|attribute| attribute.shader_location == location)?;
                        Some((layout, index))
                    })
                    .unwrap_or_else(|| {
                        panic!("{label} {entry}: no attribute at @location({location}) {name}")
                    });
                let buffer = layouts[layout];
                assert_eq!(
                    (buffer.fields[index], buffer.buffer.attributes[index].format),
                    (name.as_str(), format),
                    "{label} {entry}: @location({location})"
                );
                read[layout][index] = true;
            }
        }
        for (layout, read) in layouts.iter().zip(&read) {
            let unread: Vec<_> = layout
                .fields
                .iter()
                .zip(read)
                .filter_map(|(field, read)| (!read).then_some(*field))
                .collect();
            assert!(unread.is_empty(), "{readers:?} read no {unread:?}");
        }
    }
}

/// The `group` bindings `source` declares, by variable name.
fn declared_bindings(label: &str, source: &str, group: u32) -> Vec<(String, u32)> {
    let module = parse(label, source);
    module
        .global_variables
        .iter()
        .filter_map(|(_, variable)| {
            let binding = variable.binding.as_ref()?;
            (binding.group == group).then(|| (variable.name.clone().unwrap(), binding.binding))
        })
        .collect()
}

// A binding renumbered on one side only (a bind module's WGSL, the Rust name
// the layouts and groups bind it by, or a layout's entries), a WGSL binding
// with no Rust name, or a Rust name with no WGSL binding fails here: the
// numbers come from naga's parse of the WGSL.
#[wasm_bindgen_test(unsupported = test)]
fn rust_binding_names_match_wgsl_bindings() {
    use super::bind::{self, blended, caster, group0, group1, group2, hardware, shadow_mask};
    let named = [
        (0, "view", group0::VIEW),
        (0, "frame", group0::FRAME),
        (0, "shadow_sampler", group0::SHADOW_SAMPLER),
        (0, "backdrop_map", group0::BACKDROP_MAP),
        (0, "environment_map", group0::ENVIRONMENT_MAP),
        (0, "environment_sampler", group0::ENVIRONMENT_SAMPLER),
        (0, "lookup_tables", group0::LOOKUP_TABLES),
        (0, "lights", group0::LIGHTS),
        (0, "clusters", group0::CLUSTERS),
        (0, "decals", group0::DECALS),
        (0, "decal_atlas", group0::DECAL_ATLAS),
        (0, "decal_sampler", group0::DECAL_SAMPLER),
        (0, "baked", group0::BAKED),
        (0, "collection", group0::COLLECTION),
        (0, "local_shadow_atlas", group0::LOCAL_SHADOW_ATLAS),
        (0, "local_shadows", group0::LOCAL_SHADOWS),
        (0, "directional_shadow_map", group0::DIRECTIONAL_SHADOW_MAP),
        (0, "static_lightmap", group0::STATIC_LIGHTMAP),
        (
            0,
            "static_lightmap_direction",
            group0::STATIC_LIGHTMAP_DIRECTION,
        ),
        (0, "baked_sampler", group0::BAKED_SAMPLER),
        (
            0,
            "static_irradiance_atlas",
            group0::STATIC_IRRADIANCE_ATLAS,
        ),
        (0, "static_direction_atlas", group0::STATIC_DIRECTION_ATLAS),
        (0, "fog_volume", group0::FOG_VOLUME),
        (0, "fog_sampler", group0::FOG_SAMPLER),
        (0, "dynamic_gi_probes", group0::DYNAMIC_GI_PROBES),
        (0, "irradiance_volume", group0::IRRADIANCE_VOLUME),
        (1, "objects", group1::OBJECTS),
        (1, "scene_source", group1::SCENE_SOURCE),
        (1, "scene_instances", group1::SCENE_INSTANCES),
        (2, "material", group2::MATERIAL),
        (2, "base_map", group2::BASE_MAP),
        (2, "mr_map", group2::MR_MAP),
        (2, "tex_sampler", group2::TEX_SAMPLER),
        (2, "emission_map", group2::EMISSION_MAP),
        (2, "normal_map", group2::NORMAL_MAP),
        (2, "bump_map", group2::BUMP_MAP),
        (2, "baked_material", group2::BAKED_MATERIAL),
        (2, "anisotropy_map", group2::ANISOTROPY_MAP),
        (3, "blended_reflections", blended::REFLECTIONS),
        (3, "blended_surface_depth", blended::SURFACE_DEPTH),
        (3, "blended_trace", blended::TRACE),
        (3, "caster_positions", caster::POSITIONS),
        (3, "scene_tlas", hardware::SCENE_TLAS),
        (3, "shadow_mask", shadow_mask::MASK),
        (3, "shadow_mask_slots", shadow_mask::SLOTS),
    ];
    let numbers = |entries: &[wgpu::BindGroupLayoutEntry]| -> Vec<u32> {
        entries.iter().map(|entry| entry.binding).collect()
    };
    let layouts = [
        (
            "bind_lit",
            &[&super::BIND_LIT][..],
            0,
            numbers(&bind::lit_entries()),
        ),
        (
            "bind_unlit",
            &[&super::BIND_UNLIT],
            0,
            numbers(&bind::unlit_entries()),
        ),
        (
            "bind_shadow",
            &[&super::BIND_SHADOW],
            0,
            numbers(&bind::uniform_entries()),
        ),
        (
            "bind_scene",
            &[&super::BIND_SCENE, &super::SCENE_RAYS],
            1,
            numbers(&bind::scene_entries()),
        ),
        (
            "bind_material",
            &[&super::BIND_MATERIAL],
            2,
            numbers(&bind::material_entries()),
        ),
        (
            "bind_blended",
            &[&super::BIND_BLENDED],
            3,
            numbers(&bind::blended_entries()),
        ),
        (
            "bind_caster_positions",
            &[&super::BIND_CASTER_POSITIONS],
            3,
            numbers(&bind::caster_positions_entries()),
        ),
        (
            "scene_rays_hardware",
            &[&super::SCENE_RAYS_QUERY_OPAQUE],
            3,
            numbers(&[bind::tlas_entry()]),
        ),
        (
            "bind_shadow_mask",
            &[&super::BIND_SHADOW_MASK],
            3,
            numbers(&bind::shadow_mask_entries()),
        ),
    ];
    let mut used = vec![false; named.len()];
    for (label, modules, group, mut layout) in layouts {
        let mut declared = declared_bindings(label, &compose(modules), group);
        for (name, binding) in &declared {
            let index = named
                .iter()
                .position(|(named_group, named, _)| *named_group == group && named == name)
                .unwrap_or_else(|| panic!("{label}: group {group} `{name}` has no Rust name"));
            assert_eq!(named[index].2, *binding, "{label}: `{name}`");
            used[index] = true;
        }
        let mut declared: Vec<u32> = declared.drain(..).map(|(_, binding)| binding).collect();
        declared.sort_unstable();
        layout.sort_unstable();
        assert_eq!(layout, declared, "{label}: the Rust layout's bindings");
    }
    for ((group, name, _), used) in named.iter().zip(used) {
        assert!(used, "group {group} `{name}` is declared by no bind module");
    }
}

// The mips a probe holds (LEVELS, which sizes captures, uploads and
// readbacks) and the roughness each mip holds in the shaders that write and
// sample probes come from one constant on each side.
#[wasm_bindgen_test(unsupported = test)]
fn specular_probe_levels_match_rust() {
    let program = parse(
        "probe_prefilter",
        &compose(&[&crate::stages::probe_prefilter::PREFILTER]),
    );
    assert_eq!(
        wgsl_constant(&program, "SPECULAR_PROBE_LEVELS"),
        Some(naga::Literal::U32(crate::baked_specular_probe::LEVELS)),
    );
}

/// A Rust constant and its WGSL twin, `name` in program `program`.
pub(crate) struct Constant {
    pub program: &'static str,
    pub name: &'static str,
    pub value: naga::Literal,
}

impl Constant {
    pub fn new(program: &'static str, name: &'static str, value: naga::Literal) -> Self {
        Self {
            program,
            name,
            value,
        }
    }
}

// A constant or flag bit changed in one language only fails here.
#[wasm_bindgen_test(unsupported = test)]
fn rust_constants_match_wgsl_twins() {
    let max_probes = u32::try_from(crate::baked_specular_probe::MAX_PROBES).unwrap();
    let constants = [
        Constant::new(
            "probe_culling",
            "MAX_PROBES",
            naga::Literal::U32(max_probes),
        ),
        Constant::new(
            "geometry",
            "OBJECT_STATIC",
            naga::Literal::U32(crate::shading::uniforms::OBJECT_STATIC),
        ),
        Constant::new(
            "cull",
            "OBJECT_VISIBLE",
            naga::Literal::U32(crate::shading::uniforms::OBJECT_VISIBLE),
        ),
        Constant::new(
            "cull",
            "OBJECT_CAPTURE_VISIBLE",
            naga::Literal::U32(crate::shading::uniforms::OBJECT_CAPTURE_VISIBLE),
        ),
        Constant::new(
            "cull",
            "OBJECT_DEFORMING",
            naga::Literal::U32(crate::shading::uniforms::OBJECT_DEFORMING),
        ),
        Constant::new(
            "cull",
            "LOD_PIXELS",
            naga::Literal::F32(crate::shading::lod::LOD_PIXELS),
        ),
        Constant::new(
            "cull",
            "LOD_ROUNDING",
            naga::Literal::F32(crate::shading::lod::LOD_ROUNDING),
        ),
        Constant::new(
            "geometry",
            "SHADOW_CASCADE_OVERLAP",
            naga::Literal::F32(crate::view::cascades::SHADOW_CASCADE_OVERLAP),
        ),
    ]
    .into_iter()
    .chain(super::lights::constants())
    .chain(super::clusters::constants())
    .chain(super::dynamic_gi::constants())
    .chain(super::vertex::constants())
    .chain(super::culling::constants())
    .chain(super::packed_vertex::constants())
    .chain(crate::scene::lookup_tables::constants())
    .chain(crate::scene::rays::constants())
    .chain(crate::shading::deformation::constants())
    .chain(crate::scene::probe_grid::constants())
    .chain(crate::stages::dynamic_gi::constants())
    .chain(crate::stages::reflections::source::constants())
    .chain(crate::stages::reflections::probe_culling::constants())
    .chain(crate::stages::reflections::world::classify::constants())
    .chain(crate::stages::reflections::velvet::constants())
    .chain(crate::stages::exposure::constants())
    .chain(crate::stages::opaque::ambient_occlusion::constants())
    .chain(crate::stages::fog::constants())
    .chain(super::shadow_mask::constants())
    .chain(crate::stages::shadows::traced::denoise::constants());
    let programs = programs();
    for constant in constants {
        let (label, source) = programs
            .iter()
            .find(|(program, _)| *program == constant.program)
            .unwrap_or_else(|| panic!("no program {}", constant.program));
        assert_eq!(
            wgsl_constant(&parse(label, source), constant.name),
            Some(constant.value),
            "{}",
            constant.name
        );
    }
}
