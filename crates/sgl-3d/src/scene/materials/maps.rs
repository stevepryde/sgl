//! A material's maps: as it was authored with them, which validation reads,
//! and in effect on the device's binding tier, the one list that decides
//! what is uploaded, what group 2 binds, the ray-source texture words and
//! the record's `maps` bits (specs/sgl3d-architecture.md, Binding tiers).
use crate::asset::Material as AuthoredMaterial;
use crate::scene::rays::MaterialTextures;
use crate::shading::bind::BindingTier;
use crate::shading::bind::group2::MaterialMap;
use crate::shading::material::MaterialMaps;

/// The image `material` was authored with for `map`, an index into the
/// images added with it. Its occlusion map is SGL3D's only in its
/// metallic-roughness map's image (ORM packing); one in an image of its own
/// is not sampled (the glTF loader lists it as ignored).
pub(super) fn authored(material: &AuthoredMaterial, map: MaterialMap) -> Option<usize> {
    match map {
        MaterialMap::Base => material.base_texture,
        MaterialMap::MetallicRoughness => material.mr_texture,
        MaterialMap::Occlusion => material
            .occlusion_texture
            .filter(|_| material.packed_occlusion()),
        MaterialMap::Emission => material.emissive_texture,
        MaterialMap::Normal => material.normal_texture,
        MaterialMap::Bump => material.bump_texture,
        MaterialMap::Anisotropy => material.anisotropy_texture,
        MaterialMap::Clearcoat => material.clearcoat_texture,
        MaterialMap::CoatRoughness => material.coat_roughness_texture,
        MaterialMap::CoatNormal => material.coat_normal_texture,
        MaterialMap::Iridescence => material.iridescence_texture,
        MaterialMap::IridescenceThickness => material.iridescence_thickness_texture,
        MaterialMap::Transmission => material.transmission_texture,
        MaterialMap::Thickness => material.thickness_texture,
        MaterialMap::SheenColor => material.sheen_color_texture,
        MaterialMap::SheenRoughness => material.sheen_roughness_texture,
        MaterialMap::DiffuseTransmission => material.diffuse_transmission_texture,
        MaterialMap::DiffuseTransmissionColor => material.diffuse_transmission_color_texture,
    }
}

/// The maps `material` was authored with.
pub(super) fn authored_maps(material: &AuthoredMaterial) -> MaterialMaps {
    MaterialMaps::of(
        MaterialMap::ALL
            .into_iter()
            .filter(|&map| authored(material, map).is_some()),
    )
}

/// A material's maps in effect, each one's image or none: those it was
/// authored with whose binding a device of its tier binds, less a bump map
/// beside a normal map, which shading never takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct InEffect {
    images: [Option<usize>; MaterialMap::ALL.len()],
}

impl InEffect {
    /// `material`'s maps in effect on a device of `tier`, as indices into
    /// the images added with it.
    pub fn of(material: &AuthoredMaterial, tier: BindingTier) -> Self {
        let normal = authored(material, MaterialMap::Normal).is_some();
        let mut images = [None; MaterialMap::ALL.len()];
        for map in MaterialMap::ALL {
            let shaded = !(map == MaterialMap::Bump && normal);
            images[map as usize] =
                authored(material, map).filter(|_| shaded && map.binding().tier <= tier);
        }
        Self { images }
    }

    /// `map`'s image, where it is in effect.
    pub fn get(&self, map: MaterialMap) -> Option<usize> {
        self.images[map as usize]
    }

    /// The same maps, each image `image` maps to.
    pub fn map(self, image: impl Fn(usize) -> usize) -> Self {
        Self {
            images: self.images.map(|index| index.map(&image)),
        }
    }

    /// Each map's image and whether its binding samples it as sRGB colour.
    pub fn images(&self) -> impl Iterator<Item = (usize, bool)> + '_ {
        MaterialMap::ALL
            .into_iter()
            .filter_map(|map| Some((self.get(map)?, map.binding().colour)))
    }

    /// The image group 2 binds at `binding`: that of the map in effect that
    /// fills it, if any. Two maps fill one binding only from one image
    /// (the metallic-roughness map and its packed occlusion) or one at a
    /// time (a normal map, else a bump map).
    pub fn bound(&self, binding: u32) -> Option<usize> {
        MaterialMap::ALL
            .into_iter()
            .filter(|map| map.binding().binding == binding)
            .find_map(|map| self.get(map))
    }

    /// The record's `maps` bits.
    pub fn maps(&self) -> MaterialMaps {
        MaterialMaps::of(
            MaterialMap::ALL
                .into_iter()
                .filter(|&map| self.get(map).is_some()),
        )
    }

    /// The ray-source texture words: `word` of each map's image, zero (white)
    /// where none is in effect. The occlusion map is read from the
    /// metallic-roughness map's word; the transmission map has none, since
    /// rays pass through a transmissive surface, and the thickness map's is
    /// read by a ray hit's diffusely transmitted lobe.
    pub fn ray_textures(&self, word: impl Fn(usize) -> u32) -> MaterialTextures {
        let mut textures = MaterialTextures::default();
        for map in MaterialMap::ALL {
            let image = self.get(map).map_or(0, &word);
            match map {
                MaterialMap::Base => textures.base = image,
                MaterialMap::MetallicRoughness => textures.metallic_roughness = image,
                MaterialMap::Occlusion | MaterialMap::Transmission => {}
                MaterialMap::Thickness => textures.thickness = image,
                MaterialMap::Emission => textures.emission = image,
                MaterialMap::Normal => textures.normal = image,
                MaterialMap::Bump => textures.bump = image,
                MaterialMap::Anisotropy => textures.anisotropy = image,
                MaterialMap::Clearcoat => textures.clearcoat = image,
                MaterialMap::CoatRoughness => textures.coat_roughness = image,
                MaterialMap::CoatNormal => textures.coat_normal = image,
                MaterialMap::Iridescence => textures.iridescence = image,
                MaterialMap::IridescenceThickness => textures.iridescence_thickness = image,
                MaterialMap::SheenColor => textures.sheen_color = image,
                MaterialMap::SheenRoughness => textures.sheen_roughness = image,
                MaterialMap::DiffuseTransmission => textures.diffuse_transmission = image,
                MaterialMap::DiffuseTransmissionColor => {
                    textures.diffuse_transmission_color = image
                }
            }
        }
        textures
    }
}
