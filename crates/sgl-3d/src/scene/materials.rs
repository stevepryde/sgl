//! Materials: each one's values, raster group 2 (values, maps, sampler,
//! lightmap eligibility and map coordinates) and record in the ray source.
//! The `Scene` operations that add, read, edit and remove them are
//! `edits`'.
use super::planar_reflectors::PlanarReflectors;
use super::rays::SceneRays;
use super::render_textures::RenderTextures;
use super::shaders::Shaders;
use super::slots::Slots;
use super::textures::{self, Textures};
use super::{SceneError, buffer};
use crate::asset::{Image, Material as AuthoredMaterial, TextureWrap};
use crate::content::identity::{MaterialId, ModelId, RenderTextureId};
use crate::content::material::{AlphaMode, SurfaceMaterial};
use crate::shading::bind::group2::MaterialMap;
use crate::shading::map_uvs::MaterialBindings;
use crate::shading::material::MaterialUniform;
use group::{Bound, Groups};
use maps::{InEffect, authored, normal_wrap};
use params::ParamBlock;
use std::collections::HashMap;
use std::ops::Range;
use validate::{
    validate_alpha, validate_anisotropy, validate_core, validate_diffuse_transmission,
    validate_iridescence, validate_normal_layers, validate_reflectance, validate_shader,
    validate_sheen, validate_transmission, validate_uv_transform,
};

/// The parameter blocks of `values`' shader, of its size, where it has one.
fn params_block(
    device: &wgpu::Device,
    shaders: &Shaders,
    values: &SurfaceMaterial,
) -> Option<ParamBlock> {
    let shader = values.shader?;
    let size = shaders
        .get(shader.shader)
        .expect("a validated material's shader lives")
        .layout
        .size;
    Some(ParamBlock::new(device, size))
}

/// Whether a frame's visibility mask (all groups when None) enables the
/// visibility group `group`: every bit of it.
pub(crate) fn group_enabled(group: u32, mask: Option<u32>) -> bool {
    mask.is_none_or(|mask| group & mask == group)
}

pub(crate) struct Material {
    pub values: SurfaceMaterial,
    /// The wrapping of the normal map it was added with, which normal
    /// layers' validation reads; none without one.
    normal_wrap: Option<[TextureWrap; 2]>,
    /// Group 2.
    pub group: wgpu::BindGroup,
    /// What group 2 binds besides its buffers: its maps in effect.
    bound: Bound,
    buffer: wgpu::Buffer,
    /// Group 2's `material_bindings`: lightmap eligibility, which
    /// `set_baked` writes, and its map bindings' transforms.
    baked: wgpu::Buffer,
    casts_directional_shadow: bool,
    /// Its distinct textures.
    textures: Vec<usize>,
    /// Its record in the ray source.
    record: Range<u32>,
    /// Its use list: the models whose meshes are drawn with it, each with
    /// how many of its meshes are.
    pub users: HashMap<ModelId, u32>,
    /// Meshes drawn with it, model meshes and their levels of detail alike,
    /// without authored tangent frames.
    pub untangented: u32,
    /// Its shader's parameter blocks, while it has a shader.
    params: Option<ParamBlock>,
    /// Changes when its parameter block does (`Scene::set_shader_parameters`),
    /// so the local-light shadow faces its casters reach redraw.
    pub parameters: u64,
}

impl Material {
    /// Its record in the ray source.
    pub fn word(&self) -> u32 {
        self.record.start
    }

    /// Whether the frame's visibility mask (all groups when None) enables
    /// its visibility group.
    pub fn enabled(&self, mask: Option<u32>) -> bool {
        group_enabled(self.values.visibility_group, mask)
    }

    /// Its values as the shaders read them, with its maps in effect.
    fn uniform(&self) -> MaterialUniform {
        MaterialUniform::new(
            &self.values,
            self.bound.maps.maps(),
            &self.bound.samplers,
            &self.bound.uvs,
        )
    }

    /// The map bindings that sample its meshes' second UV set, and those
    /// that sample through a transform (`Variant::uv1`,
    /// `Variant::transformed`).
    pub fn map_uvs(&self) -> (u32, u32) {
        (self.bound.uvs.uv1(), self.bound.uvs.transformed())
    }

    /// Whether raster samples one of its maps in the shader, in a state
    /// other than the one group 2's sampler binds
    /// (`Variant::shader_sampled`).
    pub fn shader_sampled(&self) -> u32 {
        self.bound.samplers.shader_sampled()
    }

    /// Whether an occlusion map of its own or a specular or specular colour
    /// map is in effect (`Variant::occlusion_specular`).
    pub fn occlusion_specular(&self) -> bool {
        self.bound.maps.occlusion_specular()
    }

    /// Whether its shader's parameter block differs from the last
    /// submitted frame's; false without a shader.
    pub fn parameters_changed(&self) -> bool {
        self.params.as_ref().is_some_and(ParamBlock::changed)
    }

    /// Whether its record's shading changes with the frame's time
    /// (`MaterialUniform::surface_moves`), as rays see it too.
    pub fn record_moves(&self) -> bool {
        self.uniform().surface_moves()
    }

    /// Animated evaluated inputs that a masked shader may use for coverage,
    /// even when the game's function never reads a time context field.
    pub fn coverage_moves(&self) -> bool {
        matches!(self.values.alpha, AlphaMode::Mask { .. }) && self.uniform().normal_layers_move()
    }

    /// How far its shader moves a vertex, in its meshes' units; 0 without
    /// one.
    pub fn displacement_bound(&self) -> f32 {
        self.values
            .shader
            .map_or(0., |shader| shader.displacement_bound)
    }

    /// Whether its meshes cast local-light shadows under `mask` (all
    /// groups when None): those of every material but a blended one, which
    /// no shadow draws, in an enabled visibility group.
    pub fn casts_local_shadow(&self, mask: Option<u32>) -> bool {
        !self.values.blended() && self.enabled(mask)
    }

    /// Whether its shader may change what its casters cast with its
    /// parameters, the frame's time or an instance's data: it casts
    /// local-light shadows, and its vertex function moves vertices (a
    /// displacement bound above 0) or its surface function sets a masked
    /// caster's coverage.
    pub fn shader_casts(&self) -> bool {
        self.casts_local_shadow(None)
            && self.values.shader.is_some_and(|shader| {
                shader.displacement_bound > 0.
                    || matches!(self.values.alpha, AlphaMode::Mask { .. })
            })
    }

    /// Visibility and explicit casting are independent caller policies.
    pub fn casts_directional_shadow(&self, mask: u32) -> bool {
        self.casts_directional_shadow && self.enabled(Some(mask))
    }

    /// Whether it casts the directional shadow where its group is enabled.
    pub fn casts_directional_shadows(&self) -> bool {
        self.casts_directional_shadow
    }
    /// Group 2's values uniform.
    #[cfg(all(test, not(target_arch = "wasm32")))]
    pub fn uniform_buffer(&self) -> &wgpu::Buffer {
        &self.buffer
    }
}

pub(crate) struct Materials {
    pub slots: Slots<MaterialId, Material>,
    /// Changes when an edit changes what casters draw
    /// (`SurfaceMaterial::draws_other_geometry`).
    pub casters: u64,
    /// How many of its blended materials receive screen-space
    /// reflections, how many opaque or masked
    /// ones' surfaces may move from frame to frame (their records move, they
    /// have a shader or they name a render texture: `Scene::shading_changes`)
    /// and of those how many
    /// records do (`Material::record_moves`), and how many have an
    /// iridescent film.
    receivers: usize,
    moving: usize,
    moving_records: usize,
    films: usize,
    sheens: usize,
    diffuse_transmission: usize,
    transmissive: usize,
    /// How many have an occlusion map of its own or a specular or specular
    /// colour map in effect (`holds_occlusion_specular`).
    occlusion_specular: usize,
    textures: Textures,
    groups: Groups,
}

impl Materials {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        Self {
            slots: Slots::default(),
            casters: 0,
            receivers: 0,
            moving: 0,
            moving_records: 0,
            films: 0,
            sheens: 0,
            diffuse_transmission: 0,
            transmissive: 0,
            occlusion_specular: 0,
            textures: Textures::default(),
            groups: Groups::new(device, queue),
        }
    }

    /// Whether a material is a blended receiver of screen-space reflections.
    pub fn holds_receivers(&self) -> bool {
        self.receivers > 0
    }

    /// Whether an opaque or masked material's shading may change where its
    /// geometry stands still in some frame (`Scene::shading_changes`): its
    /// record moves (`Material::record_moves`), it has a shader or it names
    /// a render texture.
    pub fn holds_moving_surfaces(&self) -> bool {
        self.moving > 0
    }

    /// Whether an opaque or masked material's record moves
    /// (`Material::record_moves`), as rays see it.
    pub fn holds_moving_records(&self) -> bool {
        self.moving_records > 0
    }

    /// The device's binding tier, which a shader's programs are validated
    /// for.
    pub fn tier(&self) -> crate::shading::bind::BindingTier {
        self.groups.tier
    }

    /// Whether a material's shader may change what its casters cast
    /// (`Material::shader_casts`).
    pub fn shader_casts(&self) -> bool {
        self.slots
            .iter()
            .any(|(_, material)| material.shader_casts())
    }

    /// `id`'s parameter block of its shader's size becomes `bytes`, which
    /// changes its parameter revision where it differs.
    pub fn set_parameters(&mut self, queue: &wgpu::Queue, id: MaterialId, bytes: &[u8]) {
        let material = self.slots.get_mut(id).expect("a live material");
        let changed = material
            .params
            .as_mut()
            .expect("a material with a shader")
            .set(queue, bytes);
        if changed {
            material.parameters = super::next_generation();
        }
    }

    /// Before a frame: each shader parameter block's last submitted copy is
    /// uploaded where it changed, and the block the frame draws recorded.
    pub fn prepare_frame(&mut self, queue: &wgpu::Queue) {
        for (_, material) in self.slots.iter_mut() {
            if let Some(params) = &mut material.params {
                params.prepare_frame(queue);
            }
        }
    }

    /// Commits a submitted frame: each shader parameter block as the frame
    /// drew it becomes the one its next frame's motion is measured from.
    pub fn finish_frame(&mut self) {
        for (_, material) in self.slots.iter_mut() {
            if let Some(params) = &mut material.params {
                params.finish_frame();
            }
        }
    }

    /// Whether a material has an iridescent film: the lit pipelines
    /// evaluate films only while one does (`LitConstants::films`).
    pub fn holds_films(&self) -> bool {
        self.films > 0
    }

    /// Whether a material has a sheen: the lit pipelines evaluate sheens
    /// only while one does (`LitConstants::sheens`).
    pub fn holds_sheens(&self) -> bool {
        self.sheens > 0
    }

    /// Whether a material passes diffuse light through: the lit pipelines
    /// evaluate the transmitted lobe only while one does
    /// (`LitConstants::diffuse_transmission`).
    pub fn holds_diffuse_transmission(&self) -> bool {
        self.diffuse_transmission > 0
    }

    /// Whether a material is transmissive: the blended pipelines compile
    /// transmission in only while one is.
    pub fn holds_transmissive(&self) -> bool {
        self.transmissive > 0
    }

    /// Whether a material has an occlusion map of its own or a specular or
    /// specular colour map in effect: ray hits sample them only while one
    /// does (`LitConstants::occlusion_specular`).
    pub fn holds_occlusion_specular(&self) -> bool {
        self.occlusion_specular > 0
    }

    /// Counts `by` more materials of `values` whose record moves or not
    /// (`record_moves`): one with an iridescent film among the films, one
    /// with a sheen among the sheens, one that passes diffuse light through
    /// among those; a blended one among the receivers where it is one, else
    /// one
    /// whose surface moves (its record, with a shader or naming a render
    /// texture) among the moving and one whose record moves among those.
    fn count(&mut self, values: &SurfaceMaterial, record_moves: bool, by: isize) {
        let add = |count: &mut usize| *count = count.checked_add_signed(by).unwrap();
        if values.iridescence > 0. {
            add(&mut self.films);
        }
        if values.sheen_color.iter().any(|&channel| channel > 0.) {
            add(&mut self.sheens);
        }
        if values.diffuse_transmission > 0. {
            add(&mut self.diffuse_transmission);
        }
        if values.transmissive() {
            add(&mut self.transmissive);
        }
        if values.blended() {
            if values.receives_screen_space_reflections() {
                add(&mut self.receivers);
            }
            return;
        }
        if record_moves || values.shader.is_some() || values.emission_render_texture.is_some() {
            add(&mut self.moving);
        }
        if record_moves {
            add(&mut self.moving_records);
        }
    }

    /// The textures whose mip chains the mip chain stage has yet to build.
    pub fn chains(&mut self) -> &mut textures::Chains {
        &mut self.textures.chains
    }

    /// The texture at `index`.
    #[cfg(all(test, not(target_arch = "wasm32")))]
    pub fn texture(&self, index: usize) -> &textures::Texture {
        self.textures.get(index)
    }

    pub fn get(&self, id: MaterialId) -> Result<&Material, SceneError> {
        self.slots.get(id).ok_or(SceneError::UnknownMaterial)
    }

    pub fn get_mut(&mut self, id: MaterialId) -> Result<&mut Material, SceneError> {
        self.slots.get_mut(id).ok_or(SceneError::UnknownMaterial)
    }

    /// The texture `id`'s `map` samples, as colour or data as the map
    /// samples it, and the words of its image's record in the ray source
    /// (none without ray data), if it has the map.
    #[cfg(any(test, feature = "diagnostics"))]
    pub fn map_texture(
        &self,
        id: MaterialId,
        map: MaterialMap,
    ) -> Result<Option<(&wgpu::Texture, Range<u32>)>, SceneError> {
        Ok(self.get(id)?.bound.maps.get(map).map(|index| {
            let texture = self.textures.get(index);
            let view = if map.binding().colour {
                &texture.color
            } else {
                &texture.data
            };
            let view = view.as_ref().expect("a map's texture is made for its use");
            (view.texture(), texture.ray.clone())
        }))
    }

    /// Validates `materials` and the images they were authored with, on
    /// every binding tier alike, without uploading.
    fn validate(
        device: &wgpu::Device,
        materials: &[AuthoredMaterial],
        images: &[Image],
    ) -> Result<(), SceneError> {
        for material in materials {
            for map in MaterialMap::ALL {
                if let Some(texture) = authored(material, map) {
                    let image = images.get(texture.image).ok_or(SceneError::MissingImage)?;
                    textures::validate(device, image)?;
                    validate_uv_transform(&texture.transform)?;
                }
            }
            let values = SurfaceMaterial::authored(material);
            validate_core(&values)?;
            validate_anisotropy(&values, 0)?;
            validate_alpha(&values)?;
            validate_reflectance(&values)?;
            validate_iridescence(&values)?;
            validate_transmission(&values)?;
            validate_sheen(&values)?;
            validate_diffuse_transmission(&values)?;
            validate_normal_layers(&values, normal_wrap(material))?;
        }
        Ok(())
    }

    /// Adds `materials`, whose texture indices index `images`, sharing the
    /// textures they use. On failure nothing remains added. An added
    /// material has no shader: a game names one with `set`.
    pub fn add(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        rays: &mut SceneRays,
        materials: &[AuthoredMaterial],
        images: &[Image],
    ) -> Result<Vec<MaterialId>, SceneError> {
        Self::validate(device, materials, images)?;
        let in_effect: Vec<_> = materials
            .iter()
            .map(|material| InEffect::of(material, self.groups.tier))
            .collect();
        // How each image a map in effect samples is sampled: as colour, as
        // data.
        let mut uses = vec![[false; 2]; images.len()];
        for (index, colour) in in_effect.iter().flat_map(InEffect::images) {
            uses[index][usize::from(!colour)] = true;
        }
        for (image, &use_as) in images.iter().zip(&uses) {
            textures::validate_uses(device, image, use_as)?;
            if use_as != [false; 2] && rays.ray_data() == super::RayData::Enabled {
                textures::validate_cpu_texels(image)?;
            }
        }
        let mut uploaded = vec![None; images.len()];
        let mut added = Vec::new();
        let result = self.upload(
            device,
            queue,
            rays,
            (materials, &in_effect, images),
            &uses,
            (&mut uploaded, &mut added),
        );
        if result.is_err() {
            for &id in &added {
                self.remove(rays, id)
                    .expect("a material just added has no users");
            }
        }
        // Textures no material took are freed now.
        for texture in uploaded.into_iter().flatten() {
            self.textures.free_unused(rays, texture);
        }
        result.map(|()| added)
    }

    /// Uploads the used images into `uploaded`, then adds each material
    /// with its maps in effect into `added`, stopping at the first failure.
    fn upload(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        rays: &mut SceneRays,
        (materials, in_effect, images): (&[AuthoredMaterial], &[InEffect], &[Image]),
        uses: &[[bool; 2]],
        (uploaded, added): (&mut [Option<usize>], &mut Vec<MaterialId>),
    ) -> Result<(), SceneError> {
        for (index, (image, &use_as)) in images.iter().zip(uses).enumerate() {
            if use_as != [false; 2] {
                uploaded[index] = Some(self.textures.add(device, queue, rays, image, use_as)?);
            }
        }
        for (material, &maps) in materials.iter().zip(in_effect) {
            let maps = maps.map(|index| uploaded[index].expect("a map in effect is uploaded"));
            added.push(self.add_one(device, queue, rays, material, maps)?);
        }
        Ok(())
    }

    /// Adds `material` with `maps`, its maps in effect as texture indices.
    fn add_one(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        rays: &mut SceneRays,
        material: &AuthoredMaterial,
        maps: InEffect,
    ) -> Result<MaterialId, SceneError> {
        let values = SurfaceMaterial::authored(material);
        let samplers = maps.samplers();
        let uvs = maps.uvs();
        let uniform = MaterialUniform::new(&values, maps.maps(), &samplers, &uvs);
        let more_maps = crate::shading::material::more_maps(&samplers, &uvs);
        let record = rays.add_material(
            device,
            queue,
            &uniform,
            maps.ray_textures(|texture| self.textures.get(texture).ray.start),
            (more_maps, &uvs),
        )?;
        let values_buffer = buffer(
            device,
            "material",
            bytemuck::bytes_of(&uniform),
            wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        );
        let baked = buffer(
            device,
            "material bindings",
            bytemuck::bytes_of(&MaterialBindings {
                baked: 0,
                more_maps,
                padding: [0; 2],
                map_uvs: *uvs.rows(),
            }),
            wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        );
        let bound = Bound {
            maps,
            samplers,
            uvs,
        };
        let group = self.groups.group(
            device,
            &self.textures,
            &bound,
            [&values_buffer, &baked],
            None,
            None,
        );
        let mut distinct: Vec<usize> = maps.images().map(|(texture, _)| texture).collect();
        distinct.sort_unstable();
        distinct.dedup();
        for &texture in &distinct {
            self.textures.use_texture(texture);
        }
        self.count(&values, uniform.surface_moves(), 1);
        if maps.occlusion_specular() {
            self.occlusion_specular += 1;
        }
        Ok(self.slots.insert(Material {
            values,
            normal_wrap: normal_wrap(material),
            group,
            bound,
            buffer: values_buffer,
            baked,
            casts_directional_shadow: material.casts_directional_shadow,
            textures: distinct,
            record,
            users: HashMap::new(),
            untangented: 0,
            parameters: 0,
            params: None,
        }))
    }

    /// Samples every material's maps with at most `anisotropy` anisotropic
    /// samples (`anisotropy_clamp`), remaking their groups when it changes.
    pub fn set_anisotropy(
        &mut self,
        device: &wgpu::Device,
        render_textures: &RenderTextures,
        anisotropy: u16,
    ) {
        if !self.groups.set_anisotropy(anisotropy) {
            return;
        }
        self.regroup(device, render_textures, |_| true);
    }

    /// Remakes group 2 of the materials that name `id`, a render texture
    /// whose GPU texture was replaced.
    pub fn rebind_render_texture(
        &mut self,
        device: &wgpu::Device,
        render_textures: &RenderTextures,
        id: RenderTextureId,
    ) {
        self.regroup(device, render_textures, |material| {
            material.values.emission_render_texture == Some(id)
        });
    }

    /// Remakes group 2 of each material `which` selects.
    fn regroup(
        &mut self,
        device: &wgpu::Device,
        render_textures: &RenderTextures,
        which: impl Fn(&Material) -> bool,
    ) {
        let remade: Vec<_> = self
            .slots
            .iter()
            .filter(|(_, material)| which(material))
            .map(|(id, material)| {
                let group = self
                    .groups
                    .of(device, &self.textures, render_textures, material);
                (id, group)
            })
            .collect();
        for (id, group) in remade {
            self.slots.get_mut(id).expect("a live material").group = group;
        }
    }

    /// Whether material `id` may take `values` (`set`).
    pub fn validate_edit(
        &self,
        (shaders, render_textures, planar_reflectors): (
            &Shaders,
            &RenderTextures,
            &PlanarReflectors,
        ),
        id: MaterialId,
        values: &SurfaceMaterial,
    ) -> Result<(), SceneError> {
        validate_shader(values, shaders)?;
        if let Some(texture) = values.emission_render_texture {
            render_textures.get(texture)?;
        }
        if let Some(reflector) = values.planar_reflector {
            planar_reflectors.get(reflector)?;
            if values.blended() {
                return Err(SceneError::BlendedPlanarReflector);
            }
        }
        let material = self.slots.get(id).ok_or(SceneError::UnknownMaterial)?;
        validate_core(values)?;
        validate_anisotropy(values, material.untangented)?;
        validate_alpha(values)?;
        validate_reflectance(values)?;
        validate_iridescence(values)?;
        validate_transmission(values)?;
        validate_sheen(values)?;
        validate_diffuse_transmission(values)?;
        validate_normal_layers(values, material.normal_wrap)
    }

    /// Replaces a material's values in raster and the ray source at once;
    /// one whose shader changes takes zero parameter blocks of its new
    /// shader's size, in a group 2 that binds them, and one whose render
    /// texture changes a group 2 that binds it.
    pub fn set(
        &mut self,
        queue: &wgpu::Queue,
        (rays, shaders, render_textures, planar_reflectors): (
            &SceneRays,
            &Shaders,
            &RenderTextures,
            &PlanarReflectors,
        ),
        id: MaterialId,
        values: SurfaceMaterial,
    ) -> Result<(), SceneError> {
        self.validate_edit((shaders, render_textures, planar_reflectors), id, &values)?;
        let material = self.slots.get_mut(id).ok_or(SceneError::UnknownMaterial)?;
        if material.values != values {
            if material.values.draws_other_geometry(&values) {
                self.casters = super::next_generation();
            }
            let old = (material.values, material.record_moves());
            material.values = values;
            let uniform = material.uniform();
            crate::counters::write_buffer(queue, &material.buffer, 0, bytemuck::bytes_of(&uniform));
            rays.write_material(queue, material.word(), &uniform);
            let shader = |values: &SurfaceMaterial| values.shader.map(|shader| shader.shader);
            let device = &self.groups.device.clone();
            let reshaded = shader(&old.0) != shader(&values);
            if reshaded {
                material.params = params_block(device, shaders, &values);
            }
            if reshaded || old.0.emission_render_texture != values.emission_render_texture {
                material.group = self
                    .groups
                    .of(device, &self.textures, render_textures, material);
            }
            self.count(&old.0, old.1, -1);
            self.count(&values, uniform.surface_moves(), 1);
        }
        Ok(())
    }

    /// Whether lightmap charts light the material.
    pub fn set_baked(&self, queue: &wgpu::Queue, rays: &SceneRays, id: MaterialId, baked: bool) {
        let material = self.slots.get(id).expect("a live material");
        crate::counters::write_buffer(
            queue,
            &material.baked,
            0,
            bytemuck::cast_slice(&[u32::from(baked)]),
        );
        rays.write_baked(queue, material.word(), baked);
    }

    pub fn remove(&mut self, rays: &mut SceneRays, id: MaterialId) -> Result<(), SceneError> {
        if !self.get(id)?.users.is_empty() {
            return Err(SceneError::MaterialInUse);
        }
        let material = self.slots.remove(id).unwrap();
        self.count(&material.values, material.record_moves(), -1);
        if material.occlusion_specular() {
            self.occlusion_specular -= 1;
        }
        rays.free(material.record);
        for texture in material.textures {
            self.textures.release(rays, texture);
        }
        Ok(())
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
impl Materials {
    /// `id`'s shader parameter blocks, this frame's and the previous one,
    /// while it has a shader.
    pub fn parameter_blocks(&self, id: MaterialId) -> Option<[&wgpu::Buffer; 2]> {
        let params = self.slots.get(id)?.params.as_ref()?;
        Some([&params.current, &params.previous])
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod coat_film_tests;
mod edits;
mod group;
pub(crate) mod maps;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod normal_layer_tests;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod occlusion_specular_tests;
mod params;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod sheen_transmission_tests;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tier_tests;
mod validate;
