//! Group 0 for every view the renderer draws: the lit, unlit and shadow
//! layouts, the frame's `FrameUniform` buffer, neutral stand-ins, and the
//! groups built from them, which the renderer owns and stages borrow. The
//! scene and material layouts (groups 1 and 2) match the scene's own groups.
use super::FrameViews;
use crate::EnvironmentId;
use crate::scene::probes::UploadedProbes;
use crate::shading;
use crate::shading::bind::group0;
use crate::{Scene, shading::uniforms::FrameUniform};
use bytemuck::Zeroable;

/// The shadow maps, local-light shadow records and comparison sampler lit
/// group 0 binds, which the shadow stage draws.
pub(crate) struct ShadowMaps {
    /// The directional shadow cascades, one per layer.
    pub directional: wgpu::TextureView,
    /// The frame's local-light shadow atlas.
    pub local_atlas: wgpu::TextureView,
    /// Its static layers, which probe captures and ray hits sample.
    pub local_layers: wgpu::TextureView,
    /// Where each light's shadow is in it.
    pub local_records: wgpu::Buffer,
    pub sampler: wgpu::Sampler,
}

/// The fog volume every group 0 binds, which the fog stage writes, and the
/// sampler that filters it.
#[derive(Clone, Copy)]
pub(crate) struct FogVolume<'a> {
    pub view: &'a wgpu::TextureView,
    pub sampler: &'a wgpu::Sampler,
}

/// The local-light shadows a lit group 0 binds: an atlas and the records
/// that place each light's shadow in it.
#[derive(Clone, Copy)]
pub(crate) struct LocalShadows<'a> {
    pub atlas: &'a wgpu::TextureView,
    pub records: &'a wgpu::Buffer,
}

/// The groups that bind one scene's resources, the frame's environment and
/// the views' clusters.
struct SceneGroups {
    /// `Scene::resources` they were built from.
    resources: u64,
    /// The environment they bind, while the scene has it.
    environment: Option<EnvironmentId>,
    /// The camera's clusters, the ray hits' lists and the volume's probe
    /// hits' lists they bind.
    clusters: [wgpu::Buffer; 3],
    /// The local-light shadow records they bind.
    local_records: wgpu::Buffer,
    /// The shadow maps they bind: the directional cascades, the local-light
    /// atlas and its static layers.
    shadow_maps: [wgpu::TextureView; 3],
    /// The fog volume they bind.
    fog: wgpu::TextureView,
    /// The dynamic GI volume's probes they bind.
    dynamic_gi: wgpu::TextureView,
    /// The camera's lit group, with the installed probes, which its
    /// blended surfaces sample: source completion adds opaque surfaces'
    /// probe specular from the G-buffer, but blended surfaces write none.
    camera_lit: wgpu::BindGroup,
    /// The lit group of world-space ray hits: their light and decal lists,
    /// the static shadow layers and the installed probes, since they run no
    /// source completion and add probe specular themselves, as captures do.
    ray_hit_lit: wgpu::BindGroup,
    /// The lit group of the dynamic GI probe rays' hits: as world-space ray
    /// hits', with the volume's light and decal list.
    volume_lit: wgpu::BindGroup,
    /// The camera's sky and transient group.
    camera_unlit: wgpu::BindGroup,
}

pub(crate) struct FrameBindings {
    pub lit: wgpu::BindGroupLayout,
    pub unlit: wgpu::BindGroupLayout,
    pub shadow: wgpu::BindGroupLayout,
    /// Group 1's layout (`shading::bind::scene`).
    pub scene: wgpu::BindGroupLayout,
    /// Group 2's layout (`shading::bind::material`).
    pub material: wgpu::BindGroupLayout,
    /// The blended pipelines' group 3 (`shading::bind::blended`), which the
    /// transparent stage binds.
    pub blended: wgpu::BindGroupLayout,
    /// The lighting pass's group 3 while ray-traced shadows run
    /// (`shading::bind::shadow_mask`), which the opaque stage binds.
    pub shadow_mask: wgpu::BindGroupLayout,
    /// The GPU-built cascades' casters' group 3
    /// (`shading::bind::caster_positions`), which the scene's positions
    /// slabs bind (`scene::geometry`).
    pub caster_positions: wgpu::BindGroupLayout,
    /// The frame's `FrameUniform`, shared by every view of the frame.
    pub frame: wgpu::Buffer,
    /// No specular probes: lit groups bind this while the scene has none
    /// installed.
    pub empty_probes: UploadedProbes,
    pub shadow_maps: ShadowMaps,
    /// The fog volume every group 0 binds and its sampler, which the fog
    /// stage writes.
    fog: (wgpu::TextureView, wgpu::Sampler),
    /// The dynamic GI volume's probes every lit group 0 binds, which the
    /// dynamic GI stage writes.
    dynamic_gi: wgpu::TextureView,
    camera_view: wgpu::Buffer,
    cascades: [wgpu::BindGroup; super::cascades::MAX_SHADOW_CASCADES],
    groups: Option<SceneGroups>,
}

impl FrameBindings {
    /// Group 0 for `views` over the lit layout `lit`
    /// (`shading::bind::lit`), binding `shadow_maps`, `fog` and the
    /// `dynamic_gi` probes until a frame refreshes them.
    pub fn new(
        device: &wgpu::Device,
        lit: wgpu::BindGroupLayout,
        views: &FrameViews,
        shadow_maps: ShadowMaps,
        fog: FogVolume<'_>,
        dynamic_gi: &wgpu::TextureView,
    ) -> Self {
        let shadow = shading::bind::shadow(device);
        let frame = crate::scene::buffer(
            device,
            "camera frame",
            bytemuck::bytes_of(&FrameUniform::zeroed()),
            wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        );
        let shadow_group = |label, view: &wgpu::Buffer| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(label),
                layout: &shadow,
                entries: &shading::bind::uniforms(view, &frame),
            })
        };
        let cascades = std::array::from_fn(|index| {
            shadow_group("directional shadow cascade", &views.cascades[index].buffer)
        });
        Self {
            lit,
            unlit: shading::bind::unlit(device),
            shadow,
            scene: shading::bind::scene(device),
            material: shading::bind::material(device),
            blended: shading::bind::blended(device),
            shadow_mask: shading::bind::shadow_mask(device),
            caster_positions: shading::bind::caster_positions(device),
            frame,
            empty_probes: UploadedProbes::empty(device),
            shadow_maps,
            fog: (fog.view.clone(), fog.sampler.clone()),
            dynamic_gi: dynamic_gi.clone(),
            camera_view: views.camera.buffer.clone(),
            cascades,
            groups: None,
        }
    }

    /// Rebuilds the camera's groups when `scene` is another scene, has
    /// replaced a resource they bind, `environment` (the frame's) binds
    /// other textures, `views` replaced a cluster buffer, `shadows`
    /// replaced a map or its local-light shadow records, `fog` its volume,
    /// or the dynamic GI stage its probes (`dynamic_gi`).
    #[allow(clippy::too_many_arguments)]
    pub fn refresh(
        &mut self,
        device: &wgpu::Device,
        scene: &Scene,
        environment: Option<EnvironmentId>,
        views: &FrameViews,
        shadows: ShadowMaps,
        fog: FogVolume<'_>,
        dynamic_gi: &wgpu::TextureView,
    ) {
        self.shadow_maps = shadows;
        self.fog = (fog.view.clone(), fog.sampler.clone());
        self.dynamic_gi = dynamic_gi.clone();
        let environment = scene.environments.live(environment);
        let clusters = [
            views.clusters.buffer().clone(),
            views.ray_lists.buffer().clone(),
            views.volume_lists.buffer().clone(),
        ];
        let local_records = self.shadow_maps.local_records.clone();
        let maps = &self.shadow_maps;
        let shadow_maps = [
            maps.directional.clone(),
            maps.local_atlas.clone(),
            maps.local_layers.clone(),
        ];
        if self.groups.as_ref().is_some_and(|groups| {
            groups.resources == scene.resources
                && groups.environment == environment
                && groups.clusters == clusters
                && groups.local_records == local_records
                && groups.shadow_maps == shadow_maps
                && groups.fog == self.fog.0
                && groups.dynamic_gi == self.dynamic_gi
        }) {
            return;
        }
        let probes = scene.specular_probes().unwrap_or(&self.empty_probes);
        let camera_lit = self.lit_group(
            device,
            scene,
            environment,
            [&self.camera_view, &self.frame],
            probes,
            &clusters[0],
            self.local_shadows(),
            &self.dynamic_gi,
        );
        let ray_hit_lit = self.lit_group(
            device,
            scene,
            environment,
            [&self.camera_view, &self.frame],
            probes,
            &clusters[1],
            self.static_local_shadows(&self.shadow_maps.local_records),
            &self.dynamic_gi,
        );
        let volume_lit = self.lit_group(
            device,
            scene,
            environment,
            [&self.camera_view, &self.frame],
            probes,
            &clusters[2],
            self.static_local_shadows(&self.shadow_maps.local_records),
            &self.dynamic_gi,
        );
        let camera_unlit =
            self.unlit_group(device, scene, environment, &self.camera_view, &self.frame);
        self.groups = Some(SceneGroups {
            resources: scene.resources,
            environment,
            clusters,
            local_records,
            shadow_maps,
            fog: self.fog.0.clone(),
            dynamic_gi: self.dynamic_gi.clone(),
            camera_lit,
            ray_hit_lit,
            volume_lit,
            camera_unlit,
        });
    }

    fn groups(&self) -> &SceneGroups {
        self.groups
            .as_ref()
            .expect("FrameBindings::refresh before drawing")
    }

    pub fn camera_lit(&self) -> &wgpu::BindGroup {
        &self.groups().camera_lit
    }

    pub fn ray_hit_lit(&self) -> &wgpu::BindGroup {
        &self.groups().ray_hit_lit
    }

    /// The lit group of the dynamic GI probe rays' hits.
    pub fn volume_lit(&self) -> &wgpu::BindGroup {
        &self.groups().volume_lit
    }

    pub fn camera_unlit(&self) -> &wgpu::BindGroup {
        &self.groups().camera_unlit
    }

    /// The fog volume every group 0 binds.
    pub fn fog(&self) -> FogVolume<'_> {
        FogVolume {
            view: &self.fog.0,
            sampler: &self.fog.1,
        }
    }

    /// Directional shadow cascade `index`'s shadow group.
    pub fn cascade(&self, index: usize) -> &wgpu::BindGroup {
        &self.cascades[index]
    }

    /// The frame's local-light shadows.
    pub fn local_shadows(&self) -> LocalShadows<'_> {
        LocalShadows {
            atlas: &self.shadow_maps.local_atlas,
            records: &self.shadow_maps.local_records,
        }
    }

    /// The local-light shadows' static layers, placed by `records`: what
    /// probe captures and ray hits sample.
    pub fn static_local_shadows<'a>(&'a self, records: &'a wgpu::Buffer) -> LocalShadows<'a> {
        LocalShadows {
            atlas: &self.shadow_maps.local_layers,
            records,
        }
    }

    /// A lit group 0: `view` and `frame`, the scene's lighting, decals,
    /// irradiance volume and `environment`, `probes`, the view's `clusters`,
    /// the `local` shadows its lights take, the fog volume and the
    /// `dynamic_gi` volume's probes.
    #[allow(clippy::too_many_arguments)]
    pub fn lit_group(
        &self,
        device: &wgpu::Device,
        scene: &Scene,
        environment: Option<EnvironmentId>,
        [view, frame]: [&wgpu::Buffer; 2],
        probes: &UploadedProbes,
        clusters: &wgpu::Buffer,
        local: LocalShadows<'_>,
        dynamic_gi: &wgpu::TextureView,
    ) -> wgpu::BindGroup {
        let environments = &scene.environments;
        let baked = &scene.static_lighting;
        let texture = |binding, view| wgpu::BindGroupEntry {
            binding,
            resource: wgpu::BindingResource::TextureView(view),
        };
        let [view_entry, frame_entry] = shading::bind::uniforms(view, frame);
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("scene lighting and local environment"),
            layout: &self.lit,
            entries: &[
                view_entry,
                frame_entry,
                texture(group0::STATIC_IRRADIANCE_ATLAS, &baked.atlas.irradiance),
                texture(group0::STATIC_DIRECTION_ATLAS, &baked.atlas.direction),
                texture(group0::STATIC_LIGHTMAP, &baked.lightmap.irradiance),
                texture(group0::STATIC_LIGHTMAP_DIRECTION, &baked.lightmap.direction),
                wgpu::BindGroupEntry {
                    binding: group0::BAKED_SAMPLER,
                    resource: wgpu::BindingResource::Sampler(&baked.sampler),
                },
                texture(
                    group0::DIRECTIONAL_SHADOW_MAP,
                    &self.shadow_maps.directional,
                ),
                texture(group0::LOCAL_SHADOW_ATLAS, local.atlas),
                wgpu::BindGroupEntry {
                    binding: group0::LOCAL_SHADOWS,
                    resource: local.records.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: group0::SHADOW_SAMPLER,
                    resource: wgpu::BindingResource::Sampler(&self.shadow_maps.sampler),
                },
                texture(
                    group0::ENVIRONMENT_MAP,
                    &environments.frame(environment).pmrem,
                ),
                wgpu::BindGroupEntry {
                    binding: group0::ENVIRONMENT_SAMPLER,
                    resource: wgpu::BindingResource::Sampler(&environments.sampler),
                },
                texture(group0::LOOKUP_TABLES, &scene.lookup_tables),
                wgpu::BindGroupEntry {
                    binding: group0::LIGHTS,
                    resource: scene.lights.buffer().as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: group0::CLUSTERS,
                    resource: clusters.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: group0::DECALS,
                    resource: scene.decals.buffer().as_entire_binding(),
                },
                texture(group0::DECAL_ATLAS, &scene.decals.atlas.view),
                wgpu::BindGroupEntry {
                    binding: group0::DECAL_SAMPLER,
                    resource: wgpu::BindingResource::Sampler(&scene.decals.sampler),
                },
                texture(group0::BAKED, &probes.view),
                wgpu::BindGroupEntry {
                    binding: group0::COLLECTION,
                    resource: probes.metadata.as_entire_binding(),
                },
                texture(group0::FOG_VOLUME, &self.fog.0),
                wgpu::BindGroupEntry {
                    binding: group0::FOG_SAMPLER,
                    resource: wgpu::BindingResource::Sampler(&self.fog.1),
                },
                texture(group0::DYNAMIC_GI_PROBES, dynamic_gi),
                texture(group0::IRRADIANCE_VOLUME, scene.irradiance_cells.view()),
            ],
        })
    }

    /// An unlit group 0: `view` and `frame` with the panorama and PMREM
    /// atlas of `environment` and the fog volume.
    pub fn unlit_group(
        &self,
        device: &wgpu::Device,
        scene: &Scene,
        environment: Option<EnvironmentId>,
        view: &wgpu::Buffer,
        frame: &wgpu::Buffer,
    ) -> wgpu::BindGroup {
        let environments = &scene.environments;
        let environment = environments.frame(environment);
        let texture = |binding, view| wgpu::BindGroupEntry {
            binding,
            resource: wgpu::BindingResource::TextureView(view),
        };
        let [view_entry, frame_entry] = shading::bind::uniforms(view, frame);
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("sky and transient frame"),
            layout: &self.unlit,
            entries: &[
                view_entry,
                frame_entry,
                texture(group0::BACKDROP_MAP, &environment.backdrop),
                texture(group0::ENVIRONMENT_MAP, &environment.pmrem),
                wgpu::BindGroupEntry {
                    binding: group0::ENVIRONMENT_SAMPLER,
                    resource: wgpu::BindingResource::Sampler(&environments.sampler),
                },
                texture(group0::FOG_VOLUME, &self.fog.0),
                wgpu::BindGroupEntry {
                    binding: group0::FOG_SAMPLER,
                    resource: wgpu::BindingResource::Sampler(&self.fog.1),
                },
            ],
        })
    }

    /// A shadow group 0: `view` and `frame`.
    pub fn shadow_group(
        &self,
        device: &wgpu::Device,
        view: &wgpu::Buffer,
        frame: &wgpu::Buffer,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("static capture shadow cascade"),
            layout: &self.shadow,
            entries: &shading::bind::uniforms(view, frame),
        })
    }
}
