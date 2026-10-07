//! Materials: each one's values, raster group 2 (values, maps, sampler and
//! lightmap eligibility) and record in the ray source, with the `Scene`
//! operations that add, read, edit and remove them.
use super::candidates::CandidateMesh;
use super::rays::SceneRays;
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
use std::collections::HashMap;
use std::ops::Range;
use validate::{
    validate_alpha, validate_anisotropy, validate_iridescence, validate_normal_layers,
    validate_reflectance,
};

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

    /// Whether its shading changes with the frame's time where its geometry
    /// stands still (`MaterialUniform::surface_moves`).
    pub fn surface_moves(&self) -> bool {
        self.uniform().surface_moves()
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
    /// of those receive screen-space reflections, and how many opaque or
    /// masked ones' surfaces move (`Material::surface_moves`), and how many
    /// have an iridescent film.
    masked: usize,
    blended: usize,
    receivers: usize,
    moving: usize,
    films: usize,
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
            films: 0,
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

    /// Whether a material has an iridescent film: the lit pipelines
    /// evaluate films only while one does (`LitConstants::films`).
    pub fn holds_films(&self) -> bool {
        self.films > 0
    }

    /// Counts `by` more materials of `values` whose surface `moves` or not.
    fn count(&mut self, values: &SurfaceMaterial, moves: bool, by: isize) {
        let add = |count: &mut usize| *count = count.checked_add_signed(by).unwrap();
        let alpha = values.alpha;
        if moves && !matches!(alpha, AlphaMode::Blend { .. }) {
            add(&mut self.moving);
        }
        if values.iridescence > 0. {
            add(&mut self.films);
        }
        match alpha {
            AlphaMode::Opaque => {}
            AlphaMode::Mask { .. } => add(&mut self.masked),
            AlphaMode::Blend {
                receives_screen_space_reflections,
                ..
            } => {
                add(&mut self.blended);
                if receives_screen_space_reflections {
                    add(&mut self.receivers);
                }
            }
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
            validate_normal_layers(&values, authored_maps(material), material.wrap)?;
        }
        Ok(())
    }

    /// Adds `materials`, whose texture indices index `images`, sharing the
    /// textures they use. On failure nothing remains added.
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
        let group = self
            .groups
            .group(device, &self.textures, &bound, &values_buffer, &baked);
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
                let (buffer, baked) = (&material.buffer, &material.baked);
                let group = self
                    .groups
                    .group(device, &self.textures, bound, buffer, baked);
                (id, group)
            })
            .collect();
        for (id, group) in remade {
            self.slots.get_mut(id).expect("a live material").group = group;
        }
    }

    /// Replaces a material's values in raster and the ray source at once.
    pub fn set(
        &mut self,
        queue: &wgpu::Queue,
        rays: &SceneRays,
        id: MaterialId,
        values: SurfaceMaterial,
    ) -> Result<(), SceneError> {
        let material = self.slots.get_mut(id).ok_or(SceneError::UnknownMaterial)?;
        validate_anisotropy(&values, material.untangented)?;
        validate_alpha(&values)?;
        validate_reflectance(&values)?;
        validate_iridescence(&values)?;
        validate_normal_layers(&values, material.maps, material.bound.wrap)?;
        if material.values != values {
            if material.values.caster_values() != values.caster_values() {
                self.casters = super::next_generation();
            }
            let old = (material.values, material.surface_moves());
            material.values = values;
            let uniform = material.uniform();
            crate::counters::write_buffer(queue, &material.buffer, 0, bytemuck::bytes_of(&uniform));
            rays.write_material(queue, material.word(), &uniform);
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
        self.count(&material.values, material.surface_moves(), -1);
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
        let alpha = self.materials.get(id)?.values.alpha;
        if self.materials.get(id)?.values != values {
            self.edited();
        }
        // Blended meshes have no draw candidates: a material that becomes or
        // stops being blended adds or removes its users' instances' ones.
        let blending = matches!(alpha, AlphaMode::Blend { .. }) != values.blended();
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
        self.materials.set(queue, &self.rays, id, values)?;
        if std::mem::discriminant(&alpha) != std::mem::discriminant(&values.alpha) {
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
        self.materials.remove(&mut self.rays, id)
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod coat_film_tests;
mod group;
pub(crate) mod maps;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod normal_layer_tests;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tier_tests;
mod validate;
