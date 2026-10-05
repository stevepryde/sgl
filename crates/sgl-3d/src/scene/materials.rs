//! Materials: each one's values, raster group 2 (values, maps, sampler and
//! lightmap eligibility) and record in the ray source, with the `Scene`
//! operations that add, read, edit and remove them.
use super::candidates::CandidateMesh;
use super::rays::{MaterialTextures, SceneRays};
use super::slots::Slots;
use super::textures::{self, Textures};
use super::{Scene, SceneError, buffer};
use crate::asset::{Image, Material as AuthoredMaterial};
use crate::content::identity::{MaterialId, ModelId};
use crate::content::material::{AlphaMode, SurfaceMaterial};
use crate::shading::bind::group2;
use crate::shading::material::{MaterialMaps, MaterialUniform};
use gltf::texture::WrappingMode;
use std::collections::HashMap;
use std::ops::Range;
use validate::{validate_alpha, validate_anisotropy, validate_normal_layers};

pub(crate) struct Material {
    pub values: SurfaceMaterial,
    /// The maps it was added with.
    maps: MaterialMaps,
    /// Group 2.
    pub group: wgpu::BindGroup,
    /// What group 2 binds besides its buffers.
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

/// The maps group 2 binds, each a texture index or `None` for the white
/// fallback, and their wrapping, which its sampler takes.
struct Bound {
    base: Option<usize>,
    emission: Option<usize>,
    metallic_roughness: Option<usize>,
    normal: Option<usize>,
    bump: Option<usize>,
    anisotropy: Option<usize>,
    wrap: [WrappingMode; 2],
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

    /// Its values as the shaders read them.
    fn uniform(&self) -> MaterialUniform {
        MaterialUniform::new(&self.values, self.maps)
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
    /// of those receive screen-space reflections.
    masked: usize,
    blended: usize,
    receivers: usize,
    textures: Textures,
    /// White, for maps a material does not have.
    fallback: wgpu::TextureView,
    layout: wgpu::BindGroupLayout,
    /// The samplers' `anisotropy_clamp`.
    anisotropy: u16,
}

/// A material's maps: each one's index into the images added with it, and
/// whether it is sampled as sRGB colour rather than linear data.
fn maps(material: &AuthoredMaterial) -> [(Option<usize>, bool); 6] {
    [
        (material.base_texture, true),
        (material.emissive_texture, true),
        (material.mr_texture, false),
        (material.normal_texture, false),
        (material.bump_texture, false),
        (material.anisotropy_texture, false),
    ]
}

/// The maps `material` is added with.
fn authored_maps(material: &AuthoredMaterial) -> MaterialMaps {
    MaterialMaps::new(
        material.normal_texture.is_some(),
        material.bump_texture.is_some(),
        material.anisotropy_texture.is_some(),
    )
}

impl Materials {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        let white = image::RgbaImage::from_pixel(1, 1, image::Rgba([255; 4]));
        Self {
            slots: Slots::default(),
            casters: 0,
            masked: 0,
            blended: 0,
            receivers: 0,
            textures: Textures::default(),
            fallback: textures::upload(device, queue, &white, false),
            layout: crate::shading::bind::material(device),
            anisotropy: crate::settings::AnisotropicFiltering::default().clamp(),
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

    /// Counts `by` more materials of alpha mode `alpha`.
    fn count(&mut self, alpha: AlphaMode, by: isize) {
        let add = |count: &mut usize| *count = count.checked_add_signed(by).unwrap();
        match alpha {
            AlphaMode::Opaque => {}
            AlphaMode::Mask { .. } => add(&mut self.masked),
            AlphaMode::Blend {
                receives_screen_space_reflections,
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

    /// Validates `materials` and the images they use, without uploading.
    fn validate(
        device: &wgpu::Device,
        materials: &[AuthoredMaterial],
        images: &[Image],
    ) -> Result<(), SceneError> {
        for material in materials {
            for (index, _) in maps(material) {
                if let Some(index) = index {
                    textures::validate(device, images.get(index).ok_or(SceneError::MissingImage)?)?;
                }
            }
            let values = SurfaceMaterial::authored(material);
            validate_anisotropy(&values, 0)?;
            validate_alpha(&values)?;
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
        // How each image is sampled: as colour, as data.
        let mut uses = vec![[false; 2]; images.len()];
        for material in materials {
            for (index, colour) in maps(material) {
                if let Some(index) = index {
                    uses[index][usize::from(!colour)] = true;
                }
            }
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
            (materials, images),
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
    /// into `added`, stopping at the first failure.
    fn upload(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        rays: &mut SceneRays,
        (materials, images): (&[AuthoredMaterial], &[Image]),
        uses: &[[bool; 2]],
        (uploaded, added): (&mut [Option<usize>], &mut Vec<MaterialId>),
    ) -> Result<(), SceneError> {
        for (index, (image, &use_as)) in images.iter().zip(uses).enumerate() {
            if use_as != [false; 2] {
                uploaded[index] = Some(self.textures.add(device, queue, rays, image, use_as)?);
            }
        }
        for material in materials {
            added.push(self.add_one(device, queue, rays, material, uploaded)?);
        }
        Ok(())
    }

    fn add_one(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        rays: &mut SceneRays,
        material: &AuthoredMaterial,
        uploaded: &[Option<usize>],
    ) -> Result<MaterialId, SceneError> {
        let texture = |index: Option<usize>| index.map(|index| uploaded[index].unwrap());
        let ray_word = |index: Option<usize>| {
            texture(index).map_or(0, |texture| self.textures.get(texture).ray.start)
        };
        let values = SurfaceMaterial::authored(material);
        let map_bits = authored_maps(material);
        let uniform = MaterialUniform::new(&values, map_bits);
        let record = rays.add_material(
            device,
            queue,
            &uniform,
            MaterialTextures {
                base: ray_word(material.base_texture),
                metallic_roughness: ray_word(material.mr_texture),
                emission: ray_word(material.emissive_texture),
                normal: ray_word(material.normal_texture),
                bump: ray_word(material.bump_texture),
                anisotropy: ray_word(material.anisotropy_texture),
            },
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
            base: texture(material.base_texture),
            emission: texture(material.emissive_texture),
            metallic_roughness: texture(material.mr_texture),
            normal: texture(material.normal_texture),
            bump: texture(material.bump_texture),
            anisotropy: texture(material.anisotropy_texture),
            wrap: material.wrap,
        };
        let group = self.group(device, &bound, &values_buffer, &baked);
        let mut distinct: Vec<usize> = maps(material)
            .into_iter()
            .filter_map(|(index, _)| texture(index))
            .collect();
        distinct.sort_unstable();
        distinct.dedup();
        for &texture in &distinct {
            self.textures.use_texture(texture);
        }
        self.count(values.alpha, 1);
        Ok(self.slots.insert(Material {
            values,
            maps: map_bits,
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

    /// Group 2 of a material binding `bound`, its values `buffer` and
    /// lightmap eligibility `baked`, sampled with the current anisotropy.
    fn group(
        &self,
        device: &wgpu::Device,
        bound: &Bound,
        buffer: &wgpu::Buffer,
        baked: &wgpu::Buffer,
    ) -> wgpu::BindGroup {
        let view = |texture: Option<usize>, colour: bool| -> &wgpu::TextureView {
            match texture {
                Some(texture) => {
                    let texture = self.textures.get(texture);
                    if colour {
                        texture.color.as_ref()
                    } else {
                        texture.data.as_ref()
                    }
                    .expect("a texture is uploaded as each material samples it")
                }
                None => &self.fallback,
            }
        };
        let address = |mode| match mode {
            WrappingMode::Repeat => wgpu::AddressMode::Repeat,
            WrappingMode::MirroredRepeat => wgpu::AddressMode::MirrorRepeat,
            WrappingMode::ClampToEdge => wgpu::AddressMode::ClampToEdge,
        };
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("authored material sampler"),
            address_mode_u: address(bound.wrap[0]),
            address_mode_v: address(bound.wrap[1]),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            anisotropy_clamp: self.anisotropy,
            ..Default::default()
        });
        let entry = |binding, view| wgpu::BindGroupEntry {
            binding,
            resource: wgpu::BindingResource::TextureView(view),
        };
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("retained material"),
            layout: &self.layout,
            entries: &[
                entry(group2::ANISOTROPY_MAP, view(bound.anisotropy, false)),
                wgpu::BindGroupEntry {
                    binding: group2::BAKED_MATERIAL,
                    resource: baked.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: group2::MATERIAL,
                    resource: buffer.as_entire_binding(),
                },
                entry(group2::BASE_MAP, view(bound.base, true)),
                entry(group2::MR_MAP, view(bound.metallic_roughness, false)),
                wgpu::BindGroupEntry {
                    binding: group2::TEX_SAMPLER,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
                entry(group2::EMISSION_MAP, view(bound.emission, true)),
                entry(group2::NORMAL_MAP, view(bound.normal, false)),
                entry(group2::BUMP_MAP, view(bound.bump, false)),
            ],
        })
    }

    /// Samples every material's maps with at most `anisotropy` anisotropic
    /// samples (`anisotropy_clamp`), remaking their groups when it changes.
    pub fn set_anisotropy(&mut self, device: &wgpu::Device, anisotropy: u16) {
        if self.anisotropy == anisotropy {
            return;
        }
        self.anisotropy = anisotropy;
        let groups: Vec<_> = self
            .slots
            .iter()
            .map(|(id, material)| {
                let group = self.group(device, &material.bound, &material.buffer, &material.baked);
                (id, group)
            })
            .collect();
        for (id, group) in groups {
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
        validate_normal_layers(&values, material.maps, material.bound.wrap)?;
        if material.values != values {
            if material.values.caster_values() != values.caster_values() {
                self.casters = super::next_generation();
            }
            let old = material.values.alpha;
            material.values = values;
            let uniform = material.uniform();
            crate::counters::write_buffer(queue, &material.buffer, 0, bytemuck::bytes_of(&uniform));
            rays.write_material(queue, material.word(), &uniform);
            self.count(old, -1);
            self.count(values.alpha, 1);
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
        self.count(material.values.alpha, -1);
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
mod normal_layer_tests;
mod validate;
