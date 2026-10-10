//! Materials: each one's values, raster group 2 (values, maps, sampler and
//! lightmap eligibility) and record in the ray source, with the `Scene`
//! operations that add, read, edit and remove them.
use super::candidates::CandidateMesh;
use super::rays::SceneRays;
use super::shaders::Shaders;
use super::slots::Slots;
use super::textures::{self, Textures};
use super::{Scene, SceneError, buffer};
use crate::asset::{Image, Material as AuthoredMaterial};
use crate::content::identity::{MaterialId, ModelId};
use crate::content::material::{AlphaMode, SurfaceMaterial};
use crate::shading::bind::group2::MaterialMap;
use crate::shading::material::{MaterialMaps, MaterialUniform};
use group::{Bound, Groups};
use maps::{InEffect, authored, authored_maps};
use params::ParamBlock;
use std::collections::HashMap;
use std::ops::Range;
use validate::{
    validate_alpha, validate_anisotropy, validate_diffuse_transmission, validate_iridescence,
    validate_normal_layers, validate_reflectance, validate_shader, validate_sheen,
    validate_transmission,
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

pub(crate) struct Material {
    pub values: SurfaceMaterial,
    /// The maps it was added with, which validation reads.
    maps: MaterialMaps,
    /// Group 2.
    pub group: wgpu::BindGroup,
    /// What group 2 binds besides its buffers: its maps in effect.
    bound: Bound,
    buffer: wgpu::Buffer,
    /// Lightmap eligibility, group 2's `baked_material`.
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
        mask.is_none_or(|mask| {
            let group = self.values.visibility_group;
            group & mask == group
        })
    }

    /// Its values as the shaders read them, with its maps in effect.
    fn uniform(&self) -> MaterialUniform {
        MaterialUniform::new(&self.values, self.bound.maps.maps())
    }

    /// Whether its shading may change with the frame's time where its
    /// geometry stands still: its record's (`record_moves`), or any
    /// material's with a shader, whose functions SGL3D does not analyse
    /// (Godot b130438's `is_animated()` analyses its material's code;
    /// scene_shader_forward_clustered.cpp 250–252).
    pub fn surface_moves(&self) -> bool {
        self.record_moves() || self.values.shader.is_some()
    }

    /// Whether its record's shading changes with the frame's time
    /// (`MaterialUniform::surface_moves`), as rays see it too.
    pub fn record_moves(&self) -> bool {
        self.uniform().surface_moves()
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
    /// Changes when an edit changes a value casters read
    /// (`SurfaceMaterial::caster_values`).
    pub casters: u64,
    /// How many of its materials are masked, how many blended and how many
    /// of those receive screen-space reflections, how many opaque or masked
    /// ones' surfaces move (`Material::surface_moves`) and of those how many
    /// records do (`Material::record_moves`), and how many have an
    /// iridescent film.
    masked: usize,
    blended: usize,
    receivers: usize,
    moving: usize,
    moving_records: usize,
    films: usize,
    sheens: usize,
    diffuse_transmission: usize,
    transmissive: usize,
    textures: Textures,
    groups: Groups,
}

impl Materials {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        Self {
            slots: Slots::default(),
            casters: 0,
            masked: 0,
            blended: 0,
            receivers: 0,
            moving: 0,
            moving_records: 0,
            films: 0,
            sheens: 0,
            diffuse_transmission: 0,
            transmissive: 0,
            textures: Textures::default(),
            groups: Groups::new(device, queue),
        }
    }

    /// Whether a material is masked.
    pub fn holds_masked(&self) -> bool {
        self.masked > 0
    }

    /// Whether a material is blended.
    pub fn holds_blended(&self) -> bool {
        self.blended > 0
    }

    /// Whether a material is a blended receiver of screen-space reflections.
    pub fn holds_receivers(&self) -> bool {
        self.receivers > 0
    }

    /// Whether an opaque or masked material's surface moves
    /// (`Material::surface_moves`).
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

    /// Whether a material has a shader.
    pub fn holds_shaders(&self) -> bool {
        self.slots
            .iter()
            .any(|(_, material)| material.values.shader.is_some())
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
    /// uploaded where it changed.
    pub fn prepare_frame(&mut self, queue: &wgpu::Queue) {
        for (_, material) in self.slots.iter_mut() {
            if let Some(params) = &mut material.params {
                params.prepare_frame(queue);
            }
        }
    }

    /// Commits a submitted frame: each shader parameter block becomes the
    /// one its next frame's motion is measured from.
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

    /// Counts `by` more materials of `values` whose record moves or not
    /// (`record_moves`): one with an iridescent film among the films, one
    /// with a sheen among the sheens, one that passes diffuse light through
    /// among those; a blended or transmissive one among the blended (and the
    /// receivers where it is one), else a masked one among the masked, one
    /// whose surface moves (its record, or with a shader) among the moving
    /// and one whose record moves among those.
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
            add(&mut self.blended);
            if values.receives_screen_space_reflections() {
                add(&mut self.receivers);
            }
            return;
        }
        if record_moves || values.shader.is_some() {
            add(&mut self.moving);
        }
        if record_moves {
            add(&mut self.moving_records);
        }
        if matches!(values.alpha, AlphaMode::Mask { .. }) {
            add(&mut self.masked);
        }
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

    /// Validates `materials` and the images they were authored with, on
    /// every binding tier alike, without uploading.
    fn validate(
        device: &wgpu::Device,
        materials: &[AuthoredMaterial],
        images: &[Image],
    ) -> Result<(), SceneError> {
        for material in materials {
            for map in MaterialMap::ALL {
                if let Some(index) = authored(material, map) {
                    textures::validate(device, images.get(index).ok_or(SceneError::MissingImage)?)?;
                }
            }
            let values = SurfaceMaterial::authored(material);
            validate_anisotropy(&values, 0)?;
            validate_alpha(&values)?;
            validate_reflectance(&values)?;
            validate_iridescence(&values)?;
            validate_transmission(&values)?;
            validate_sheen(&values)?;
            validate_diffuse_transmission(&values)?;
            validate_normal_layers(&values, authored_maps(material), material.wrap)?;
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
        let uniform = MaterialUniform::new(&values, maps.maps());
        let record = rays.add_material(
            device,
            queue,
            &uniform,
            maps.ray_textures(|texture| self.textures.get(texture).ray.start),
            material.wrap,
        )?;
        let values_buffer = buffer(
            device,
            "material",
            bytemuck::bytes_of(&uniform),
            wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        );
        let baked = buffer(
            device,
            "baked material eligibility",
            bytemuck::cast_slice(&[0u32; 4]),
            wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        );
        let bound = Bound {
            maps,
            wrap: material.wrap,
        };
        let group = self.groups.group(
            device,
            &self.textures,
            &bound,
            [&values_buffer, &baked],
            None,
        );
        let mut distinct: Vec<usize> = maps.images().map(|(texture, _)| texture).collect();
        distinct.sort_unstable();
        distinct.dedup();
        for &texture in &distinct {
            self.textures.use_texture(texture);
        }
        self.count(&values, uniform.surface_moves(), 1);
        Ok(self.slots.insert(Material {
            values,
            maps: authored_maps(material),
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
    pub fn set_anisotropy(&mut self, device: &wgpu::Device, anisotropy: u16) {
        if self.groups.anisotropy == anisotropy {
            return;
        }
        self.groups.anisotropy = anisotropy;
        let remade: Vec<_> = self
            .slots
            .iter()
            .map(|(id, material)| {
                let bound = &material.bound;
                let buffers = [&material.buffer, &material.baked];
                let group = self.groups.group(
                    device,
                    &self.textures,
                    bound,
                    buffers,
                    material.params.as_ref(),
                );
                (id, group)
            })
            .collect();
        for (id, group) in remade {
            self.slots.get_mut(id).expect("a live material").group = group;
        }
    }

    /// Replaces a material's values in raster and the ray source at once;
    /// one whose shader changes takes zero parameter blocks of its new
    /// shader's size, in a group 2 that binds them.
    pub fn set(
        &mut self,
        queue: &wgpu::Queue,
        (rays, shaders): (&SceneRays, &Shaders),
        id: MaterialId,
        values: SurfaceMaterial,
    ) -> Result<(), SceneError> {
        validate_shader(&values, shaders)?;
        let material = self.slots.get_mut(id).ok_or(SceneError::UnknownMaterial)?;
        validate_anisotropy(&values, material.untangented)?;
        validate_alpha(&values)?;
        validate_reflectance(&values)?;
        validate_iridescence(&values)?;
        validate_transmission(&values)?;
        validate_sheen(&values)?;
        validate_diffuse_transmission(&values)?;
        validate_normal_layers(&values, material.maps, material.bound.wrap)?;
        if material.values != values {
            if material.values.caster_values() != values.caster_values() {
                self.casters = super::next_generation();
            }
            let old = (material.values, material.record_moves());
            material.values = values;
            let uniform = material.uniform();
            crate::counters::write_buffer(queue, &material.buffer, 0, bytemuck::bytes_of(&uniform));
            rays.write_material(queue, material.word(), &uniform);
            let shader = |values: &SurfaceMaterial| values.shader.map(|shader| shader.shader);
            if shader(&old.0) != shader(&values) {
                let device = &self.groups.device;
                material.params = params_block(device, shaders, &values);
                material.group = self.groups.group(
                    device,
                    &self.textures,
                    &material.bound,
                    [&material.buffer, &material.baked],
                    material.params.as_ref(),
                );
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
            bytemuck::cast_slice(&[u32::from(baked), 0, 0, 0]),
        );
        rays.write_baked(queue, material.word(), baked);
    }

    pub fn remove(&mut self, rays: &mut SceneRays, id: MaterialId) -> Result<(), SceneError> {
        if !self.get(id)?.users.is_empty() {
            return Err(SceneError::MaterialInUse);
        }
        let material = self.slots.remove(id).unwrap();
        self.count(&material.values, material.record_moves(), -1);
        rays.free(material.record);
        for texture in material.textures {
            self.textures.release(rays, texture);
        }
        Ok(())
    }
}

impl Scene {
    /// Adds `materials`, whose texture indices index `images`. Materials
    /// added together share the textures they use; a texture lives while a
    /// material uses it. A decoded image's mips are filtered here; a
    /// compressed one's are uploaded as stored. Returns their identities in
    /// order.
    pub fn add_materials(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        materials: &[AuthoredMaterial],
        images: &[Image],
    ) -> Result<Vec<MaterialId>, SceneError> {
        self.edited();
        let ids = self
            .materials
            .add(device, queue, &mut self.rays, materials, images);
        // A failure after the source grew still leaves it grown.
        self.refresh_scene_group(device);
        ids
    }

    /// A material's current values.
    pub fn material(&self, id: MaterialId) -> Result<SurfaceMaterial, SceneError> {
        Ok(self.materials.get(id)?.values)
    }

    /// Replaces a material's values, its alpha mode among them. Anisotropy
    /// needs authored tangent frames on every mesh drawn with the material.
    /// Textures stay as added.
    pub fn set_material(
        &mut self,
        queue: &wgpu::Queue,
        id: MaterialId,
        values: SurfaceMaterial,
    ) -> Result<(), SceneError> {
        let old = self.materials.get(id)?.values;
        if old != values {
            self.edited();
        }
        // Blended and transmissive meshes have no draw candidates: a
        // material that becomes or stops being either adds or removes its
        // users' instances' ones.
        let blending = old.blended() != values.blended();
        let users: Vec<ModelId> = self.materials.get(id)?.users.keys().copied().collect();
        if blending {
            // Each user model's instances, in the order they are placed
            // again below, take the material's meshes as blended or not.
            let meshes: Vec<Vec<CandidateMesh>> = users
                .iter()
                .map(|&model| {
                    let mut meshes = self.candidate_meshes(model, self.drawn_model(model));
                    let owner = &self.drawn_model(model).meshes;
                    for (mesh, shape) in owner.iter().zip(&mut meshes) {
                        if mesh.material == id {
                            shape.blended = values.blended();
                        }
                    }
                    meshes
                })
                .collect();
            let plan: Vec<_> = users
                .iter()
                .zip(&meshes)
                .map(|(&model, meshes)| (model, meshes.as_slice(), None))
                .collect();
            if !self.candidates_fit(&plan) {
                return Err(SceneError::DeviceLimit);
            }
        }
        self.materials
            .set(queue, (&self.rays, &self.shaders), id, values)?;
        let shader = |values: &SurfaceMaterial| values.shader.map(|shader| shader.shader);
        self.shaders.count(shader(&old), -1);
        self.shaders.count(shader(&values), 1);
        if blending || std::mem::discriminant(&old.alpha) != std::mem::discriminant(&values.alpha) {
            self.models.classify_users(id, &self.materials);
        }
        if blending {
            self.place_candidates_of(&users);
        }
        self.candidates
            .material_changed(id, super::candidates::look(self.materials.get(id)?));
        Ok(())
    }

    /// Removes a material no model's mesh uses.
    pub fn remove_material(&mut self, id: MaterialId) -> Result<(), SceneError> {
        self.edited();
        let shader = self.materials.get(id)?.values.shader;
        self.materials.remove(&mut self.rays, id)?;
        self.shaders.count(shader.map(|shader| shader.shader), -1);
        Ok(())
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod coat_film_tests;
mod group;
pub(crate) mod maps;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod normal_layer_tests;
mod params;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod sheen_transmission_tests;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tier_tests;
mod validate;
