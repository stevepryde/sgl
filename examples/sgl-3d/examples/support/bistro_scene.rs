//! Example-owned content shared by the native and WebGPU Bistro consumers.
use serde_json::{Value, json};
use sgl_3d::{
    Backdrop, BakedSpecularProbe, Camera, Decal, DecalId, DecalImageId, DirectionalLight,
    DirectionalShadow, DynamicGiVolume, EnvironmentLight, Exposure, Fog, FogVolume, FrameInput,
    Light, LightCookie, LightId, LightShape, LocalExposure, Scene, Sky, SpecularProbeBox,
    SpecularProbeRadiance, SpecularProbeTexels, asset,
    glam::{Mat4, Quat, Vec3, camera},
    settings::{
        AmbientOcclusionQuality, AnisotropicFiltering, Antialiasing, Bloom, DynamicGiQuality,
        FsrQuality, RayTracedShadowQuality, ReflectionMethod, SceneResolution,
        ScreenSpaceReflections, Settings, ShadowQuality, SmaaQuality, WorldSpaceReflections,
    },
};

#[path = "puddle_mask.rs"]
mod puddle_mask;
#[path = "window_panes.rs"]
mod window_panes;

pub const SCENE: &str = "amazon-bistro-exterior-v1";
pub const CAMERAS: [(&str, Vec3, Vec3); 4] = [
    (
        "street",
        Vec3::new(-10.8, 2.5, 6.),
        Vec3::new(-2., 2.2, -4.),
    ),
    (
        "bike",
        Vec3::new(-10.2, 2.3, 3.8),
        Vec3::new(-7.3, 1.35, 0.4),
    ),
    ("overview", Vec3::new(-16., 8., 15.), Vec3::new(0., 2., -4.)),
    ("bevy", Vec3::new(-20., 10., -10.), Vec3::new(-3., 4., 0.)),
];

pub fn model_scale(descriptor: &Value) -> Result<f32, String> {
    let scale = descriptor.get("scale").map_or(Ok(1.), |value| {
        value.as_f64().ok_or("scale must be numeric")
    })? as f32;
    if !scale.is_finite() || scale <= 0. {
        return Err("fixture scale must be finite and positive".into());
    }
    Ok(scale)
}

pub fn selected_cameras(descriptor: Option<&Value>, requested: &str) -> Result<Vec<Value>, String> {
    let default_fixture;
    let source = if let Some(descriptor) = descriptor {
        descriptor
    } else {
        default_fixture = fixture();
        &default_fixture
    };
    let cameras = source["cameras"]
        .as_array()
        .ok_or("fixture cameras missing")?;
    let mut selected = Vec::new();
    for camera in cameras {
        let id = camera["id"]
            .as_str()
            .filter(|id| {
                !id.is_empty()
                    && id
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
            })
            .ok_or("camera id must be an alphanumeric basename")?;
        if requested != "all" && id != requested {
            continue;
        }
        // Keep the original native all-view behavior when no fixture was supplied.
        if descriptor.is_none() && requested == "all" && id == "bevy" {
            continue;
        }
        selected.push(camera.clone());
    }
    if selected.is_empty() {
        return Err(format!("fixture camera not found: {requested}"));
    }
    Ok(selected)
}

pub fn resolve_settings(
    base: Settings,
    descriptor: Option<&Value>,
    override_settings: Option<&Value>,
    aa: Option<Antialiasing>,
) -> Result<Settings, String> {
    let value = override_settings.or_else(|| descriptor.map(|d| &d["settings"]));
    let mut settings = value.map_or(Ok(base), |value| {
        serde_json::from_value(value.clone()).map_err(|e| e.to_string())
    })?;
    if let Some(aa) = aa {
        settings.antialiasing = aa;
    }
    Ok(settings)
}

pub fn compressed_format(manifest: &Value) -> Result<asset::CompressedFormat, String> {
    if manifest["schema"] != 1 {
        return Err("unknown compressed-image manifest schema".into());
    }
    match manifest["format"].as_str() {
        Some("bc7") => Ok(asset::CompressedFormat::Bc7),
        Some("etc2") => Ok(asset::CompressedFormat::Etc2Rgba8),
        Some("astc4x4") => Ok(asset::CompressedFormat::Astc4x4),
        _ => Err("compressed manifest needs bc7, etc2 or astc4x4".into()),
    }
}

/// Bounds images the caller owns before glTF's geometry+image accounting.
/// Source/transfer totals remain separate from this decoded-image allowance.
pub struct SuppliedImages {
    budget: asset::LoadBudget,
    pub used: u64,
}
impl SuppliedImages {
    pub fn new(budget: asset::LoadBudget) -> Self {
        Self { budget, used: 0 }
    }
    pub fn decode(
        &mut self,
        manifest: &Value,
        entry: &Value,
        encoded: &[u8],
        companion: Option<&[u8]>,
    ) -> Result<asset::CompressedImage, String> {
        if encoded.len() as u64 > self.budget.source_bytes() {
            return Err("KTX2 source budget exceeded before companion expansion".into());
        }
        let expected = compressed_format(manifest)?;
        let reader = ktx2::Reader::new(encoded).map_err(|e| e.to_string())?;
        let chain_bytes = reader.levels().try_fold(0u64, |sum, level| {
            sum.checked_add(level.uncompressed_byte_length)
                .ok_or("compressed chain byte count overflow")
        })?;
        let companion_bytes = if let Some(encoded) = companion {
            let (width, height) = image::ImageReader::new(std::io::Cursor::new(encoded))
                .with_guessed_format()
                .map_err(|e| e.to_string())?
                .into_dimensions()
                .map_err(|e| e.to_string())?;
            if width == 0
                || height == 0
                || width > 16384
                || height > 16384
                || entry["width"].as_u64() != Some(u64::from(width))
                || entry["height"].as_u64() != Some(u64::from(height))
            {
                return Err("compressed companion dimensions mismatch".into());
            }
            u64::from(width) * u64::from(height) * 4
        } else {
            0
        };
        let forecast = chain_bytes
            .checked_add(companion_bytes)
            .ok_or("supplied image byte count overflow")?;
        let remaining = self
            .budget
            .decoded_bytes()
            .checked_sub(self.used)
            .ok_or("supplied image allowance underflow")?;
        if forecast > remaining || remaining == 0 {
            return Err(format!(
                "supplied-image decoded budget exceeded before expansion: requested {forecast} bytes, {remaining} remaining, {} already owned",
                self.used
            ));
        }
        let budget = asset::LoadBudget::new(self.budget.source_bytes(), remaining)
            .map_err(|e| e.to_string())?;
        let image = if let Some(encoded_companion) = companion {
            let decoded = image::load_from_memory(encoded_companion)
                .map_err(|e| e.to_string())?
                .into_rgba8();
            asset::CompressedImage::from_ktx2_with_decoded_level_0_and_budget(
                encoded, decoded, budget,
            )
        } else {
            asset::CompressedImage::from_ktx2_with_budget(encoded, budget)
        }
        .map_err(|e| e.to_string())?;
        if image.format != expected
            || entry["width"].as_u64() != Some(u64::from(image.width))
            || entry["height"].as_u64() != Some(u64::from(image.height))
        {
            return Err("compressed image differs from manifest format/dimensions".into());
        }
        let actual = image
            .levels
            .iter()
            .try_fold(0u64, |sum, level| {
                sum.checked_add(level.len() as u64)
                    .ok_or("image byte count overflow")
            })?
            .checked_add(
                image
                    .decoded_level_0
                    .as_ref()
                    .map_or(0, |i| i.as_raw().len() as u64),
            )
            .ok_or("image byte count overflow")?;
        if actual != forecast {
            return Err("compressed image allocation differs from its validated forecast".into());
        }
        self.used = self
            .used
            .checked_add(actual)
            .ok_or("owned image byte count overflow")?;
        Ok(image)
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub fn asset_path(directory: &std::path::Path, name: &str) -> Result<std::path::PathBuf, String> {
    let basename = name.strip_prefix("Compressed/").unwrap_or(name);
    if basename.is_empty()
        || [".", ".."].contains(&basename)
        || !basename
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    {
        return Err("asset must be a basename or Compressed/basename".into());
    }
    let directory = directory.canonicalize().map_err(|e| e.to_string())?;
    if name.starts_with("Compressed/") {
        let metadata =
            std::fs::symlink_metadata(directory.join("Compressed")).map_err(|e| e.to_string())?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err("Compressed must be a real directory, not a symlink".into());
        }
    }
    let path = directory.join(name);
    let metadata = std::fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("asset must be a regular file, not a symlink or device".into());
    }
    Ok(path)
}

pub fn settings(max_quality: bool, no_ray_shadows: bool) -> Settings {
    let mut settings = Settings {
        antialiasing: Antialiasing::Taa,
        atmosphere: false,
        ..Default::default()
    };
    if max_quality {
        settings.antialiasing = Antialiasing::Fsr2;
        settings.fsr_quality = FsrQuality::NativeAa;
        settings.scene_resolution = SceneResolution::Full;
        settings.anisotropic_filtering = AnisotropicFiltering::X16;
        settings.smaa_quality = SmaaQuality::Ultra;
        settings.bloom = Bloom::On;
        settings.shadow_quality = ShadowQuality::High;
        settings.ambient_occlusion = AmbientOcclusionQuality::Ultra;
        settings.screen_space_reflections = ScreenSpaceReflections::Full;
        settings.reflection_method = ReflectionMethod::Velvet;
        settings.world_space_reflections = WorldSpaceReflections::All;
        settings.hardware_ray_tracing = true;
        settings.ray_traced_shadows = !no_ray_shadows;
        settings.ray_traced_shadow_quality = RayTracedShadowQuality::High;
        settings.dynamic_gi = DynamicGiQuality::High;
    }
    settings
}

/// Indices of the fixture's named materials: each name must match exactly
/// one material, once.
fn named_materials(
    materials: &[asset::Material],
    names: &Value,
    key: &str,
) -> Result<Vec<usize>, String> {
    let names = names
        .as_array()
        .ok_or_else(|| format!("{key} must be an array of unique material names"))?;
    let mut indices = Vec::new();
    for value in names {
        let name = value
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| format!("{key} entries must be nonempty names"))?;
        let matches: Vec<_> = materials
            .iter()
            .enumerate()
            .filter(|(_, m)| m.name == name)
            .map(|(i, _)| i)
            .collect();
        if matches.len() != 1 {
            return Err(format!(
                "{key} material {name:?} must match exactly once; found {}",
                matches.len()
            ));
        }
        if indices.contains(&matches[0]) {
            return Err(format!("duplicate {key} material {name:?}"));
        }
        indices.push(matches[0]);
    }
    Ok(indices)
}

/// Explicit content authoring, not a heuristic by emission, texture or alpha
/// mode. `transmissive_materials` makes named bulb shells dielectric
/// transmitting surfaces; their emission, textures and geometry stay intact
/// and opaque fixtures and walls remain casters.
/// `diffuse_transmission_materials` (`names`, `factor`, `color`) gives named
/// thin materials such as leaves `KHR_materials_diffuse_transmission`: they
/// pass `factor` of the light they diffuse to their other side, coloured by
/// their own base colour (factor and map) times `color`.
/// `ssr_receiver_materials` makes named glass a blended surface that
/// receives screen-space reflections and keeps its specular at full strength
/// (`AlphaMode::Blend`), so windows reflect the street in front of them.
/// Every list resolves completely, including name ambiguity, before anything
/// changes.
pub fn apply_material_authoring(
    materials: &mut [asset::Material],
    descriptor: &Value,
) -> Result<Value, String> {
    let transmissive = match descriptor.get("transmissive_materials") {
        Some(names) => named_materials(materials, names, "transmissive_materials")?,
        None => Vec::new(),
    };
    let receivers = match descriptor.get("ssr_receiver_materials") {
        Some(names) => named_materials(materials, names, "ssr_receiver_materials")?,
        None => Vec::new(),
    };
    let diffuse = descriptor
        .get("diffuse_transmission_materials")
        .map(|spec| -> Result<_, String> {
            let fields = Fields {
                value: spec,
                kind: "diffuse_transmission_materials",
            };
            let factor = fields.number("factor", None)?;
            if factor > 1. {
                return Err("diffuse_transmission_materials factor must be within 0..=1".into());
            }
            let color = fields.color("color")?;
            let indices =
                named_materials(materials, &spec["names"], "diffuse_transmission_materials")?;
            Ok((indices, factor, color))
        })
        .transpose()?;
    for &index in &transmissive {
        // KHR_materials_transmission: metals absorb refracted light, so changing
        // transmission alone on the source's metallic bulbs is not glass.
        materials[index].metallic = 0.;
        materials[index].transmission = 1.;
    }
    let mut report = json!({"count":transmissive.len(),
        "names":transmissive.iter().map(|&i| &materials[i].name).collect::<Vec<_>>(),
        "metallic":0.,"transmission":1.});
    if let Some((indices, factor, color)) = diffuse {
        for &index in &indices {
            let material = &mut materials[index];
            material.diffuse_transmission = factor;
            material.diffuse_transmission_color =
                std::array::from_fn(|c| color[c] * material.base[c]);
            material.diffuse_transmission_color_texture = material.base_texture;
        }
        report["diffuse_transmission"] = json!({"count":indices.len(),
            "names":indices.iter().map(|&i| &materials[i].name).collect::<Vec<_>>(),
            "factor":factor,"color":color,"color_texture":"base colour map"});
    }
    if !receivers.is_empty() {
        for &index in &receivers {
            materials[index].alpha = sgl_3d::AlphaMode::Blend {
                receives_screen_space_reflections: true,
                keeps_specular: true,
            };
        }
        report["ssr_receivers"] = json!({"count":receivers.len(),
            "names":receivers.iter().map(|&i| &materials[i].name).collect::<Vec<_>>(),
            "alpha":format!("{:?}", materials[receivers[0]].alpha)});
    }
    Ok(report)
}

/// One authored fixture object's fields: finite vectors and finite,
/// nonnegative numbers, with an optional default.
struct Fields<'a> {
    value: &'a Value,
    kind: &'a str,
}

impl Fields<'_> {
    fn vector(&self, name: &str) -> Result<Vec3, String> {
        let vector = Vec3::from_array(
            serde_json::from_value(self.value[name].clone())
                .map_err(|e| format!("{} {name}: {e}", self.kind))?,
        );
        if !vector.is_finite() {
            return Err(format!("{} {name} must be finite", self.kind));
        }
        Ok(vector)
    }

    fn number(&self, name: &str, fallback: Option<f32>) -> Result<f32, String> {
        let n = match self.value.get(name) {
            Some(v) => v
                .as_f64()
                .ok_or_else(|| format!("{} {name} must be numeric", self.kind))?
                as f32,
            None => fallback.ok_or_else(|| format!("{} {name} missing", self.kind))?,
        };
        if !n.is_finite() || n < 0. {
            return Err(format!(
                "{} {name} must be finite and nonnegative",
                self.kind
            ));
        }
        Ok(n)
    }

    fn color(&self, name: &str) -> Result<[f32; 3], String> {
        let color = self.vector(name)?;
        if color.min_element() < 0. {
            return Err(format!("{} {name} must be nonnegative", self.kind));
        }
        Ok(color.to_array())
    }

    fn direction(&self, name: &str) -> Result<Vec3, String> {
        let direction = self.vector(name)?;
        if direction.length_squared() == 0. {
            return Err(format!("{} {name} must be nonzero", self.kind));
        }
        Ok(direction)
    }
}

/// A pattern a fixture's spot projects as its cookie, generated in code.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CookiePattern {
    /// `"window"`: a window's panes (`window_panes`).
    Window,
}

impl CookiePattern {
    fn image(self) -> asset::Image {
        match self {
            Self::Window => asset::Image::Rgba8(window_panes::window_panes()),
        }
    }
}

/// An authored spot's cookie: its pattern and the direction of its top.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AuthoredCookie {
    pub pattern: CookiePattern,
    pub up: Vec3,
}

/// An authored light and, for a spot, the pattern it projects.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AuthoredLight {
    pub light: Light,
    pub cookie: Option<AuthoredCookie>,
}

/// Adds the fixture's local lights to `scene`, each spot's cookie with it
/// unless `cookies` is false (for cookie-on and cookie-off captures of one
/// fixture), each pattern added once as a decal image. Returns the lights'
/// identities.
pub fn add_local_lights(
    scene: &mut Scene,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    descriptor: &Value,
    cookies: bool,
) -> Result<Vec<LightId>, String> {
    let mut images = Vec::new();
    let mut ids = Vec::new();
    for authored in local_lights_from_json(descriptor)? {
        let mut light = authored.light;
        if let Some(cookie) = authored.cookie.filter(|_| cookies) {
            let image = match images
                .iter()
                .find(|(pattern, _)| *pattern == cookie.pattern)
            {
                Some(&(_, image)) => image,
                None => {
                    let image = scene
                        .add_decal_image(cookie.pattern.image())
                        .map_err(|e| e.to_string())?;
                    images.push((cookie.pattern, image));
                    image
                }
            };
            light.cookie = Some(LightCookie {
                image,
                up: cookie.up,
            });
        }
        ids.push(
            scene
                .add_light(device, queue, light)
                .map_err(|e| e.to_string())?,
        );
    }
    Ok(ids)
}

/// Example-owned authored point, spot and rectangle lights, in world-space
/// metres, degrees and linear RGB, a spot's with the pattern it projects.
/// Absent lists preserve fixtures without them. This is not an
/// emissive-material conversion: the author chooses which geometry emits
/// light and where each light shines.
pub fn local_lights_from_json(descriptor: &Value) -> Result<Vec<AuthoredLight>, String> {
    let mut lights = Vec::new();
    for (key, kind) in [
        ("point_lights", "point light"),
        ("spot_lights", "spot light"),
        ("rect_lights", "rect light"),
    ] {
        let Some(values) = descriptor.get(key) else {
            continue;
        };
        for value in values
            .as_array()
            .ok_or_else(|| format!("{key} must be an array"))?
        {
            let fields = Fields { value, kind };
            let shape = match key {
                "point_lights" => LightShape::Point {
                    radius: fields.number("radius", Some(LightShape::DEFAULT_RADIUS))?,
                },
                "spot_lights" => {
                    let inner = fields.number("inner_angle_degrees", None)?;
                    let outer = fields.number("outer_angle_degrees", None)?;
                    if inner > outer || outer >= 90. {
                        return Err(
                            "spot light needs inner <= outer angle, both below 90 degrees".into(),
                        );
                    }
                    LightShape::Spot {
                        direction: fields.direction("direction")?,
                        inner_angle: inner.to_radians(),
                        outer_angle: outer.to_radians(),
                        radius: LightShape::DEFAULT_RADIUS,
                    }
                }
                _ => {
                    let direction = fields.direction("direction")?;
                    let width_axis = fields.direction("width_axis")?;
                    let (width, height) = (
                        fields.number("width", None)?,
                        fields.number("height", None)?,
                    );
                    if width == 0. || height == 0. {
                        return Err("rect light width and height must be positive".into());
                    }
                    if width_axis.normalize().cross(direction.normalize()).length() < 1e-3 {
                        return Err("rect light width_axis must not parallel its direction".into());
                    }
                    LightShape::Rect {
                        direction,
                        width_axis,
                        width,
                        height,
                    }
                }
            };
            let range = fields.number("range", None)?;
            if range == 0. {
                return Err(format!("{kind} range must be positive"));
            }
            let casts_shadow = value.get("casts_shadow").map_or(Ok(false), |v| {
                v.as_bool()
                    .ok_or_else(|| format!("{kind} casts_shadow must be boolean"))
            })?;
            let cookie = match value.get("cookie") {
                None => None,
                Some(_) if key != "spot_lights" => {
                    return Err(format!("{kind} cannot project a cookie"));
                }
                Some(cookie) => {
                    let fields = Fields {
                        value: cookie,
                        kind: "spot light cookie",
                    };
                    let pattern = match cookie["pattern"].as_str() {
                        Some("window") => CookiePattern::Window,
                        _ => return Err("spot light cookie pattern must be \"window\"".into()),
                    };
                    Some(AuthoredCookie {
                        pattern,
                        up: fields.direction("up")?,
                    })
                }
            };
            lights.push(AuthoredLight {
                light: Light {
                    position: fields.vector("position")?,
                    shape,
                    color: fields.color("color")?,
                    intensity: fields.number("intensity", None)?,
                    range,
                    casts_shadow,
                    fog_energy: fields.number("fog_energy", Some(Light::default().fog_energy))?,
                    ..Default::default()
                },
                cookie,
            });
        }
    }
    Ok(lights)
}

/// Example-owned boxes of fog medium (`Scene::update_fog_volumes`), axis
/// aligned, in world-space metres and extinction per metre. An absent list
/// adds none. They show only where a camera's `fog` turns the atmosphere on.
pub fn fog_volumes_from_json(descriptor: &Value) -> Result<Vec<FogVolume>, String> {
    let Some(values) = descriptor.get("fog_volumes") else {
        return Ok(Vec::new());
    };
    let defaults = FogVolume::default();
    values
        .as_array()
        .ok_or("fog_volumes must be an array")?
        .iter()
        .map(|value| {
            let fields = Fields {
                value,
                kind: "fog volume",
            };
            let size = fields.vector("size")?;
            if size.min_element() <= 0. {
                return Err("fog volume size must be positive".into());
            }
            Ok(FogVolume {
                center: fields.vector("center")?,
                size,
                density: fields.number("density", None)?,
                edge_fade: fields.number("edge_fade", Some(defaults.edge_fade))?,
                ..defaults
            })
        })
        .collect()
}

/// An authored puddle (`decals`): a decal box over the ground, turned `yaw`
/// radians about +Y, that projects the generated puddle mask
/// (`puddle_mask`) down onto what it covers. Where the mask covers a
/// surface, its normal turns level (the water fills the joints between
/// stones), its roughness moves toward `roughness` and its base colour
/// darkens toward black by `darken`, all in proportion to the coverage, so
/// the stone's relief returns over the puddle's soft shore.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WetPatch {
    pub position: Vec3,
    pub size: Vec3,
    pub yaw: f32,
    pub roughness: f32,
    pub darken: f32,
    /// `None` takes `Decal::new`'s.
    pub upper_fade: Option<f32>,
    pub lower_fade: Option<f32>,
    pub normal_fade: f32,
}

impl WetPatch {
    fn decal(&self, mask: DecalImageId, flat: DecalImageId, roughness: DecalImageId) -> Decal {
        let defaults = Decal::new(mask);
        Decal {
            position: self.position,
            rotation: Quat::from_rotation_y(self.yaw),
            size: self.size,
            normal: Some(flat),
            metallic_roughness: Some(roughness),
            base_color_mix: self.darken,
            upper_fade: self.upper_fade.unwrap_or(defaults.upper_fade),
            lower_fade: self.lower_fade.unwrap_or(defaults.lower_fade),
            normal_fade: self.normal_fade,
            ..defaults
        }
    }
}

/// The fixture's puddles, `decals`, in world-space metres and degrees.
/// Each has `pattern` `"puddle"`, `position`, positive `size`, optional
/// `yaw_degrees` (default 0), `roughness` and `darken` in 0..=1, and
/// optional `upper_fade`/`lower_fade` (default `Decal::new`'s) and
/// `normal_fade` in 0..1 (default 0). An absent list adds none.
pub fn decals_from_json(descriptor: &Value) -> Result<Vec<WetPatch>, String> {
    let Some(values) = descriptor.get("decals") else {
        return Ok(Vec::new());
    };
    values
        .as_array()
        .ok_or("decals must be an array")?
        .iter()
        .map(|value| {
            let fields = Fields {
                value,
                kind: "decal",
            };
            if value["pattern"] != "puddle" {
                return Err("decal pattern must be \"puddle\"".into());
            }
            let size = fields.vector("size")?;
            if size.min_element() <= 0. {
                return Err("decal size must be positive".into());
            }
            let yaw = match value.get("yaw_degrees") {
                None => 0.,
                Some(v) => v
                    .as_f64()
                    .map(|n| n as f32)
                    .filter(|n| n.is_finite())
                    .ok_or("decal yaw_degrees must be a finite number")?
                    .to_radians(),
            };
            let unit = |name: &str, fallback: Option<f32>| -> Result<f32, String> {
                let n = fields.number(name, fallback)?;
                if n > 1. {
                    return Err(format!("decal {name} must be within 0..=1"));
                }
                Ok(n)
            };
            let normal_fade = unit("normal_fade", Some(0.))?;
            if normal_fade >= 1. {
                return Err("decal normal_fade must be below 1".into());
            }
            Ok(WetPatch {
                position: fields.vector("position")?,
                size,
                yaw,
                roughness: unit("roughness", None)?,
                darken: unit("darken", None)?,
                upper_fade: value
                    .get("upper_fade")
                    .map(|_| fields.number("upper_fade", None))
                    .transpose()?,
                lower_fade: value
                    .get("lower_fade")
                    .map(|_| fields.number("lower_fade", None))
                    .transpose()?,
                normal_fade,
            })
        })
        .collect()
}

/// Adds the fixture's puddles to `scene` as decals: the puddle mask and the
/// flat normal once, and one uniform roughness image per distinct roughness. Returns the
/// decals' identities.
pub fn add_decals(
    scene: &mut Scene,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    descriptor: &Value,
) -> Result<Vec<DecalId>, String> {
    let patches = decals_from_json(descriptor)?;
    if patches.is_empty() {
        return Ok(Vec::new());
    }
    let mask = scene
        .add_decal_image(asset::Image::Rgba8(puddle_mask::puddle_mask()))
        .map_err(|e| e.to_string())?;
    let flat = scene
        .add_decal_image(asset::Image::Rgba8(puddle_mask::flat_normal()))
        .map_err(|e| e.to_string())?;
    let mut roughness_images: Vec<(u32, DecalImageId)> = Vec::new();
    let mut ids = Vec::new();
    for patch in patches {
        let key = patch.roughness.to_bits();
        let roughness = match roughness_images.iter().find(|(k, _)| *k == key) {
            Some(&(_, image)) => image,
            None => {
                let image = scene
                    .add_decal_image(asset::Image::Rgba8(puddle_mask::wet_roughness(
                        patch.roughness,
                    )))
                    .map_err(|e| e.to_string())?;
                roughness_images.push((key, image));
                image
            }
        };
        ids.push(
            scene
                .add_decal(device, queue, patch.decal(mask, flat, roughness))
                .map_err(|e| e.to_string())?,
        );
    }
    Ok(ids)
}

/// Read back the decals the scene holds rather than report the fixture's.
pub fn decal_report(scene: &Scene, ids: &[DecalId]) -> Result<Value, String> {
    let decals = ids
        .iter()
        .map(|&id| {
            let decal = scene.decal(id).map_err(|e| e.to_string())?;
            Ok(json!({"position":decal.position.to_array(),"size":decal.size.to_array(),
                "yaw_degrees":decal.rotation.to_euler(sgl_3d::glam::EulerRot::YXZ).0.to_degrees(),
                "base_color_mix":decal.base_color_mix,"metallic_roughness_map":decal.metallic_roughness.is_some(),
                "normal_map":decal.normal.is_some(),"upper_fade":decal.upper_fade,"lower_fade":decal.lower_fade,"normal_fade":decal.normal_fade}))
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(json!({"registered":decals.len(),"decals":decals}))
}

/// Read back retained light state rather than report the requested JSON count.
pub fn local_light_report(scene: &sgl_3d::Scene, ids: &[sgl_3d::LightId]) -> Result<Value, String> {
    let mut active = 0;
    let mut shadow_casting = 0;
    for &id in ids {
        let light = scene.light(id).map_err(|e| e.to_string())?;
        if light.intensity > 0. && light.color.iter().any(|&c| c > 0.) {
            active += 1;
            if light.casts_shadow && light.shadow_opacity > 0.001 {
                shadow_casting += 1;
            }
        }
    }
    Ok(json!({"registered":ids.len(),"active":active,"shadow_casting":shadow_casting}))
}

/// Actual atlas occupancy and work, kept separate from authored light counts.
/// Warmup is deliberately excluded from measured totals, but the first frame
/// remains visible so a static cache cannot hide its population cost.
#[derive(Default)]
pub struct LocalShadowMeasurements {
    first: Option<sgl_3d::LocalShadowStats>,
    measured: Vec<sgl_3d::LocalShadowStats>,
}

impl LocalShadowMeasurements {
    pub fn observe(&mut self, stats: sgl_3d::LocalShadowStats, measured: bool) {
        self.first.get_or_insert(stats);
        if measured {
            self.measured.push(stats);
        }
    }

    pub fn report(&self) -> Value {
        let frame = |s: sgl_3d::LocalShadowStats| {
            json!({
                "shadowed":s.shadowed,"atlas_overflow":s.unshadowed,
                "layers_drawn":s.layers_drawn,"faces_drawn":s.faces_drawn,"draws":s.draws
            })
        };
        let range = |get: fn(&sgl_3d::LocalShadowStats) -> usize| {
            let values: Vec<_> = self.measured.iter().map(get).collect();
            json!({"min":values.iter().min(),"max":values.iter().max(),"total":values.iter().sum::<usize>()})
        };
        json!({"first_frame":self.first.map(frame),"measured_frames":self.measured.len(),
            "last_measured_frame":self.measured.last().copied().map(frame),
            "shadowed":range(|s|s.shadowed),"atlas_overflow":range(|s|s.unshadowed),
            "layers_drawn":range(|s|s.layers_drawn),"faces_drawn":range(|s|s.faces_drawn),"draws":range(|s|s.draws),
            "scope":"Visible shadow-casting lights only; atlas overflow is lit without shadows. Warmup and capture frames excluded from measured totals."})
    }
}

pub fn rect_light() -> Light {
    Light {
        position: Vec3::new(-7.2, 4.5, 3.),
        shape: LightShape::Rect {
            direction: Vec3::new(0., -0.7, -1.),
            width_axis: Vec3::X,
            width: 3.,
            height: 2.,
        },
        color: [1., 0.88, 0.72],
        intensity: 12.,
        range: 15.,
        casts_shadow: true,
        ..Default::default()
    }
}

/// Explicit authored volume input, shared by the native CLI and browser descriptor.
pub fn volume_from_json(value: &Value) -> Result<DynamicGiVolume, String> {
    Ok(DynamicGiVolume {
        origin: Vec3::from_array(
            serde_json::from_value(value["origin"].clone()).map_err(|e| e.to_string())?,
        ),
        spacing: Vec3::from_array(
            serde_json::from_value(value["spacing"].clone()).map_err(|e| e.to_string())?,
        ),
        probes: serde_json::from_value(value["probes"].clone()).map_err(|e| e.to_string())?,
    })
}

/// One explicitly authored outdoor local-reflection prototype, not an automatic bake.
pub fn probe_from_json(value: &Value, rgba16: &[u8]) -> Result<BakedSpecularProbe, String> {
    let (face_size, expected_bytes) = probe_shape(value)?;
    if rgba16.len() != expected_bytes {
        return Err(format!(
            "probe payload requires {expected_bytes} bytes, got {}",
            rgba16.len()
        ));
    }
    let vector = |v: &Value| -> Result<Vec3, String> {
        Ok(Vec3::from_array(
            serde_json::from_value(v.clone()).map_err(|e| e.to_string())?,
        ))
    };
    let bounds = |v: &Value| -> Result<SpecularProbeBox, String> {
        Ok(SpecularProbeBox {
            min: vector(&v["min"])?,
            max: vector(&v["max"])?,
        })
    };
    Ok(BakedSpecularProbe {
        center: vector(&value["center"])?,
        world_to_local: Mat4::IDENTITY,
        influence: bounds(&value["influence"])?,
        blend: vector(&value["blend"])?,
        proxy: value
            .get("proxy")
            .filter(|v| !v.is_null())
            .map(bounds)
            .transpose()?,
        radiance: SpecularProbeRadiance {
            face_size,
            texels: SpecularProbeTexels::Rgba16Float(
                rgba16
                    .chunks_exact(2)
                    .map(|b| u16::from_le_bytes([b[0], b[1]]))
                    .collect(),
            ),
        },
    })
}

/// The fixture's authored specular probe placements, `specular_probes`, in
/// order, each with a valid shape. An absent list authors none.
pub fn specular_probe_placements(descriptor: &Value) -> Result<Vec<&Value>, String> {
    let Some(values) = descriptor.get("specular_probes") else {
        return Ok(Vec::new());
    };
    let values = values
        .as_array()
        .ok_or("specular_probes must be an array")?;
    for value in values {
        probe_shape(value)?;
        probe_payload_name(value)?;
    }
    Ok(values.iter().collect())
}

/// A placement's `rgba16` payload basename.
pub fn probe_payload_name(value: &Value) -> Result<&str, String> {
    value["rgba16"]
        .as_str()
        .filter(|n| {
            !n.is_empty()
                && !n.starts_with('.')
                && n.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        })
        .ok_or_else(|| "probe rgba16 must be a basename".into())
}

/// The probes a scene accepted from `Scene::set_baked_specular_probes`, with
/// their payload basenames: the scene keeps no accessor for them, so this
/// reports the uploaded values, not the fixture's text.
pub fn specular_probe_report(names: &[&str], probes: &[BakedSpecularProbe]) -> Value {
    let probes: Vec<_> = names.iter().zip(probes).map(|(name, probe)| {
        let bounds = |b: &SpecularProbeBox| json!({"min":b.min.to_array(),"max":b.max.to_array()});
        json!({"rgba16":name,"center":probe.center.to_array(),"influence":bounds(&probe.influence),
            "blend":probe.blend.to_array(),"proxy":probe.proxy.as_ref().map(bounds),"face_size":probe.radiance.face_size})
    }).collect();
    json!({"registered":probes.len(),"probes":probes})
}

pub fn probe_shape(value: &Value) -> Result<(u32, usize), String> {
    if value["schema"] != 1 || value["encoding"] != "rgba16f-le" || value["levels"] != 7 {
        return Err("probe needs schema 1, rgba16f-le encoding and seven levels".into());
    }
    let face_size = u32::try_from(
        value["face_size"]
            .as_u64()
            .ok_or("probe face_size missing")?,
    )
    .map_err(|_| "invalid probe face_size")?;
    if ![64, 128, 256].contains(&face_size) {
        return Err("example probe face_size must be 64, 128 or 256".into());
    }
    let bytes = (0..7)
        .map(|level| ((face_size >> level).pow(2) * 6 * 8) as usize)
        .sum();
    Ok((face_size, bytes))
}

pub fn gi_volume() -> DynamicGiVolume {
    DynamicGiVolume {
        origin: Vec3::new(-22., -1., -18.),
        spacing: Vec3::splat(2.),
        probes: [19, 8, 17],
    }
}

pub fn frame(
    name: &str,
    eye: Vec3,
    look_at: Vec3,
    size: [u32; 2],
    exposure: f32,
    max_quality: bool,
) -> FrameInput {
    let mut frame = FrameInput::new(Camera {
        eye,
        view: camera::rh::view::look_at_mat4(eye, look_at, Vec3::Y),
        projection: sgl_3d::perspective(55f32.to_radians(), size[0] as f32 / size[1] as f32, 0.1),
    });
    frame.diffuse_environment = EnvironmentLight {
        intensity: if name == "bevy" { 0.7 } else { 1. },
        yaw: 0.,
    };
    frame.reflection_environment = frame.diffuse_environment;
    frame.backdrop = Backdrop::Environment {
        yaw: 0.,
        brightness: 1.,
    };
    frame.directional_lights[0] = Some(DirectionalLight {
        direction: if name == "bevy" {
            Vec3::new(-0.15, -1.15, 1.)
        } else {
            Vec3::new(0.4, -1., -0.3)
        },
        color: [1., 0.94, 0.85],
        illuminance: if name == "bevy" { 4.5 } else { 3. },
        shadow: Some(DirectionalShadow {
            distance: 60.,
            cascades: if max_quality { 4 } else { 3 },
        }),
        ..Default::default()
    });
    frame.exposure = Exposure {
        stops: exposure,
        automatic: None,
    };
    frame
}

/// The same authored camera/light recipe for a native or browser comparison.
pub fn frame_from_json(
    spec: &Value,
    size: [u32; 2],
    exposure: f32,
    cascades: Option<u32>,
) -> Result<FrameInput, String> {
    let vector = |value: &Value| -> Result<Vec3, String> {
        let v = Vec3::from_array(serde_json::from_value(value.clone()).map_err(|e| e.to_string())?);
        if !v.is_finite() {
            return Err("nonfinite fixture vector".into());
        }
        Ok(v)
    };
    let number = |value: &Value, fallback: f32| -> Result<f32, String> {
        let n = value.as_f64().map_or(fallback, |n| n as f32);
        if !n.is_finite() {
            return Err("nonfinite fixture number".into());
        }
        Ok(n)
    };
    let eye = vector(&spec["eye"])?;
    let target = vector(&spec["look_at"])?;
    let mut frame = FrameInput::new(Camera {
        eye,
        view: camera::rh::view::look_at_mat4(eye, target, Vec3::Y),
        projection: sgl_3d::perspective(
            number(&spec["fov_y_degrees"], 55.)?.to_radians(),
            size[0] as f32 / size[1] as f32,
            number(&spec["near"], 0.1)?,
        ),
    });
    frame.diffuse_environment = EnvironmentLight {
        intensity: number(&spec["environment_intensity"], 1.)?,
        yaw: number(&spec["environment_yaw"], 0.)?,
    };
    frame.reflection_environment = EnvironmentLight {
        intensity: number(
            &spec["reflection_environment_intensity"],
            frame.diffuse_environment.intensity,
        )?,
        ..frame.diffuse_environment
    };
    frame.backdrop = Backdrop::Environment {
        yaw: frame.diffuse_environment.yaw,
        brightness: number(&spec["backdrop_brightness"], 1.)?,
    };
    // A camera's fog turns its atmosphere on; without one it stays off.
    if let Some(fog) = spec.get("fog") {
        let fields = Fields {
            value: fog,
            kind: "fog",
        };
        let defaults = Fog::default();
        let anisotropy = fog.get("anisotropy").map_or(Ok(defaults.anisotropy), |v| {
            v.as_f64()
                .map(|n| n as f32)
                .filter(|n| (-0.9..=0.9).contains(n))
                .ok_or("fog anisotropy must be within -0.9..=0.9")
        })?;
        frame.atmosphere = true;
        frame.fog = Fog {
            density: fields.number("density", None)?,
            anisotropy,
            length: fields.number("length", Some(defaults.length))?,
            start: fields.number("start", Some(defaults.start))?,
            ..defaults
        };
    }
    let sun = &spec["sun"];
    let authored_cascades = sun
        .get("shadow_cascades")
        .map(|value| {
            u32::try_from(
                value
                    .as_u64()
                    .ok_or("shadow_cascades must be an unsigned integer")?,
            )
            .map_err(|_| "shadow_cascades exceeds u32")
        })
        .transpose()?;
    let cascades = cascades.or(authored_cascades).unwrap_or(3);
    if !(1..=4).contains(&cascades) {
        return Err("shadow cascades must be 1..=4".into());
    }
    frame.directional_lights[0] = Some(DirectionalLight {
        direction: vector(&sun["direction"])?,
        color: vector(&sun["color"])?.to_array(),
        illuminance: number(&sun["illuminance"], 3.)?,
        shadow: (sun["shadow"] != false).then_some(DirectionalShadow {
            distance: number(&sun["shadow_distance"], 60.)?,
            cascades,
        }),
        ..Default::default()
    });
    frame.exposure = Exposure {
        stops: exposure,
        automatic: None,
    };
    frame.ambient_occlusion_radius = number(&spec["ambient_occlusion_radius"], 0.5)?;
    frame.screen_space_indirect_radius = number(&spec["screen_space_indirect_radius"], 5.)?;
    Ok(frame)
}

/// The fixture's optional `local_exposure`,
/// `{ "highlight_contrast": f, "shadow_contrast": f }`, each 0..=1 and 1
/// where left out; without the key, none.
pub fn local_exposure_from_json(descriptor: &Value) -> Result<LocalExposure, String> {
    let Some(value) = descriptor.get("local_exposure") else {
        return Ok(LocalExposure::default());
    };
    let fields = Fields {
        value,
        kind: "local_exposure",
    };
    let contrast = |name| {
        let c = fields.number(name, Some(1.))?;
        if c > 1. {
            return Err(format!("local_exposure {name} must be within 0..=1"));
        }
        Ok(c)
    };
    Ok(LocalExposure {
        highlight_contrast: contrast("highlight_contrast")?,
        shadow_contrast: contrast("shadow_contrast")?,
    })
}

/// The fixture's optional `sky`: the procedural sky (`FrameEnvironment::Sky`)
/// in place of the captured panorama, lit by each camera's sun. Its fields
/// are `Sky`'s, each optional with `Sky::default()`'s value: `ground_height`
/// (world Y, metres), `rayleigh`, `mie`, `ozone`, `sun_disc`,
/// `aerial_perspective`. Without the key, none.
pub fn sky_from_json(descriptor: &Value) -> Result<Option<Sky>, String> {
    let Some(value) = descriptor.get("sky") else {
        return Ok(None);
    };
    let fields = Fields { value, kind: "sky" };
    let d = Sky::default();
    let ground_height = match value.get("ground_height") {
        None => d.ground_height,
        Some(v) => v
            .as_f64()
            .map(|n| n as f32)
            .filter(|n| n.is_finite())
            .ok_or("sky ground_height must be a finite number")?,
    };
    Ok(Some(Sky {
        ground_height,
        rayleigh: fields.number("rayleigh", Some(d.rayleigh))?,
        mie: fields.number("mie", Some(d.mie))?,
        ozone: fields.number("ozone", Some(d.ozone))?,
        sun_disc: fields.number("sun_disc", Some(d.sun_disc))?,
        aerial_perspective: fields.number("aerial_perspective", Some(d.aerial_perspective))?,
        ..d
    }))
}

/// The frame's applied fog: null while its atmosphere is off.
pub fn fog_report(frame: &FrameInput) -> Value {
    if !frame.atmosphere {
        return Value::Null;
    }
    json!({"density":frame.fog.density,"anisotropy":frame.fog.anisotropy,"length":frame.fog.length,"start":frame.fog.start})
}

/// Applied frame inputs for an authoring sidecar, kept distinct from fixture intent.
pub fn frame_metadata(frame: &FrameInput) -> Value {
    let lights: Vec<_> = frame.directional_lights.iter().map(|light| light.map(|light| {
        json!({"direction":light.direction.to_array(),"color":light.color,
            "illuminance":light.illuminance,"angular_diameter":light.angular_diameter,
            "shadow_opacity":light.shadow_opacity,"fog_energy":light.fog_energy,
            "shadow":light.shadow.map(|shadow|json!({"distance":shadow.distance,"cascades":shadow.cascades}))})
    })).collect();
    let backdrop = match frame.backdrop {
        Backdrop::Environment { yaw, brightness } => {
            json!({"environment":{"yaw":yaw,"brightness":brightness}})
        }
        Backdrop::Color(color) => json!({"color":color}),
    };
    json!({"camera":{"eye":frame.camera.eye.to_array(),"view":frame.camera.view.to_cols_array(),"projection":frame.camera.projection.to_cols_array()},
        "directional_lights":lights,
        "hemisphere_light":{"sky_color":frame.hemisphere_light.sky_color,"ground_color":frame.hemisphere_light.ground_color,"intensity":frame.hemisphere_light.intensity},
        "diffuse_environment":{"intensity":frame.diffuse_environment.intensity,"yaw":frame.diffuse_environment.yaw},
        "reflection_environment":{"intensity":frame.reflection_environment.intensity,"yaw":frame.reflection_environment.yaw},
        "backdrop":backdrop,"exposure_stops":frame.exposure.stops,
        "local_exposure":{"highlight_contrast":frame.local_exposure.highlight_contrast,"shadow_contrast":frame.local_exposure.shadow_contrast},
        "ambient_occlusion_radius_m":frame.ambient_occlusion_radius,
        "screen_space_indirect_radius_m":frame.screen_space_indirect_radius,"baked_lighting":frame.baked_lighting,
        "visibility_mask":frame.visibility_mask,"elapsed_seconds":frame.elapsed_seconds,"frame_time_ms":frame.frame_time_ms})
}

/// A small fixture descriptor, kept with the example rather than renderer policy.
/// Other embedded-GLB benchmarks can supply the same fields to the browser consumer.
pub fn fixture() -> Value {
    let mut baseline = settings(false, false);
    baseline.dynamic_gi = DynamicGiQuality::Off;
    baseline.scene_resolution = SceneResolution::Full;
    let cameras: Vec<_> = CAMERAS.iter().map(|(name, eye, look_at)| {
        let frame = frame(name, *eye, *look_at, [1280,720], 0., false);
        let sun = frame.directional_lights[0].unwrap();
        json!({"id":name,"eye":eye.to_array(),"look_at":look_at.to_array(),"fov_y_degrees":55.,"near":0.1,"ambient_occlusion_radius":frame.ambient_occlusion_radius,
            "screen_space_indirect_radius":frame.screen_space_indirect_radius,
            "sun":{"direction":sun.direction.to_array(),"color":sun.color,"illuminance":sun.illuminance,"shadow_distance":60.,"shadow_cascades":3},
            "environment_intensity":frame.diffuse_environment.intensity,"environment_yaw":0.,"backdrop_brightness":1.})
    }).collect();
    json!({"schema":1,"scene":SCENE,"asset":"BistroExterior.glb",
        "environment":{"metadata":"environment.json","panorama":"panorama.png","pmrem":"pmrem.rgba16"},
        "source_metadata":"asset-stats.json","cameras":cameras,"settings":baseline,
        "default_camera":"street","size":[1280,720],"warmup_frames":60,"exposure_stops":0.,
        "lighting":"Supplied San Giuseppe Bridge HDRI via Three WebGPU PMREM; authored directional sun; no imported scene lights or baked GI",
        "provenance":{"title":"Amazon Lumberyard Bistro","creator":"Amazon Lumberyard","license":"CC BY 4.0",
            "source":"https://developer.nvidia.com/orca/amazon-lumberyard-bistro",
            "conversion":"https://github.com/qian-o/GLTF-Assets/tree/816a1d6533c55787b3fabc9b75660c0d0c57ebc6/Bistro",
            "environment":"https://polyhaven.com/a/san_giuseppe_bridge","environment_creator":"Greg Zaal","environment_license":"CC0"},
        "notices":["LICENSE.txt","SGL-ATTRIBUTION.txt","THREE-LICENSE.txt"]})
}

#[cfg(test)]
#[allow(dead_code)]
#[path = "../../../../crates/sgl-3d/tests/fixtures/ktx2_writer.rs"]
mod ktx2_writer;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transmissive_authoring_changes_only_named_shells_and_is_atomic() {
        let shell = asset::Material {
            name: "Bulb".into(),
            metallic: 1.,
            transmission: 0.,
            emissive: [3., 2., 1.],
            ..Default::default()
        };
        let wall = asset::Material {
            name: "Wall".into(),
            ..Default::default()
        };
        let original = vec![shell.clone(), wall.clone()];
        let mut materials = original.clone();
        let report =
            apply_material_authoring(&mut materials, &json!({"transmissive_materials":["Bulb"]}))
                .unwrap();
        assert_eq!(report["count"], 1);
        assert_eq!(report["names"], json!(["Bulb"]));
        assert_eq!(materials[0].metallic, 0.);
        assert_eq!(materials[0].transmission, 1.);
        assert_eq!(materials[0].emissive, shell.emissive);
        assert_eq!(materials[0].base, shell.base);
        assert_eq!(materials[1].metallic, wall.metallic);
        assert_eq!(materials[1].transmission, wall.transmission);
        for names in [
            json!(["Bulb", "Missing"]),
            json!(["Bulb", "Bulb"]),
            json!(null),
            json!([""]),
            json!([1]),
        ] {
            let mut materials = original.clone();
            assert!(
                apply_material_authoring(&mut materials, &json!({"transmissive_materials":names}))
                    .is_err()
            );
            assert_eq!(materials[0].metallic, 1.);
            assert_eq!(materials[0].transmission, 0.);
        }
        // A bad diffuse transmission entry fails before the bulb list applies.
        for leaves in [
            json!({"names":["Missing"],"factor":0.5,"color":[1,1,1]}),
            json!({"names":["Wall"],"factor":1.5,"color":[1,1,1]}),
            json!({"names":["Wall"],"factor":0.5}),
        ] {
            let mut materials = original.clone();
            assert!(
                apply_material_authoring(
                    &mut materials,
                    &json!({"transmissive_materials":["Bulb"],"diffuse_transmission_materials":leaves})
                )
                .is_err()
            );
            assert_eq!(materials[0].transmission, 0.);
            assert_eq!(materials[1].diffuse_transmission, 0.);
        }
        let mut duplicate = vec![shell.clone(), shell];
        assert!(
            apply_material_authoring(&mut duplicate, &json!({"transmissive_materials":["Bulb"]}))
                .is_err()
        );
        assert_eq!(
            apply_material_authoring(&mut materials, &json!({})).unwrap()["count"],
            0
        );
    }

    #[test]
    fn shadow_measurements_separate_population_from_measured_cache_work() {
        let mut observations = LocalShadowMeasurements::default();
        observations.observe(
            sgl_3d::LocalShadowStats {
                shadowed: 111,
                unshadowed: 2,
                layers_drawn: 666,
                faces_drawn: 666,
                draws: 1400,
            },
            false,
        );
        observations.observe(
            sgl_3d::LocalShadowStats {
                shadowed: 110,
                unshadowed: 3,
                ..Default::default()
            },
            true,
        );
        observations.observe(
            sgl_3d::LocalShadowStats {
                shadowed: 109,
                unshadowed: 4,
                layers_drawn: 1,
                faces_drawn: 2,
                draws: 8,
            },
            true,
        );
        let report = observations.report();
        assert_eq!(report["first_frame"]["faces_drawn"], 666);
        assert_eq!(report["measured_frames"], 2);
        assert_eq!(report["faces_drawn"], json!({"min":0,"max":2,"total":2}));
        assert_eq!(report["atlas_overflow"], json!({"min":3,"max":4,"total":7}));
        assert_eq!(report["last_measured_frame"]["shadowed"], 109);
        let empty = LocalShadowMeasurements::default().report();
        assert_eq!(empty["first_frame"], Value::Null);
        assert_eq!(empty["shadowed"], json!({"min":null,"max":null,"total":0}));
    }

    #[test]
    fn authored_point_lights_keep_world_space_units_and_reject_invalid_inputs() {
        assert!(local_lights_from_json(&json!({})).unwrap().is_empty());
        let light = json!({"position":[-1.8,3.87,-7.52],"color":[1.,0.72,0.45],
            "intensity":8.,"range":5.,"radius":0.07,"casts_shadow":true});
        let parsed = local_lights_from_json(&json!({"point_lights":[light.clone()]})).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].light.position, Vec3::new(-1.8, 3.87, -7.52));
        assert_eq!(parsed[0].light.shape, LightShape::Point { radius: 0.07 });
        assert_eq!(parsed[0].light.intensity, 8.);
        assert_eq!(parsed[0].light.range, 5.);
        assert_eq!(parsed[0].light.color, [1., 0.72, 0.45]);
        assert!(parsed[0].light.casts_shadow);
        for (field, invalid) in [
            ("position", json!([0., 1.])),
            ("color", json!([-1., 1., 1.])),
            ("intensity", json!("8")),
            ("intensity", json!(1e100)),
            ("position", json!([1e100, 0., 0.])),
            ("range", json!(-1)),
            ("range", json!(0)),
            ("radius", json!(-0.1)),
            ("casts_shadow", json!("false")),
        ] {
            let mut invalid_light = light.clone();
            invalid_light[field] = invalid;
            assert!(
                local_lights_from_json(&json!({"point_lights":[invalid_light]})).is_err(),
                "{field}"
            );
        }
        assert!(local_lights_from_json(&json!({"point_lights":null})).is_err());
        let mut defaults = light;
        defaults.as_object_mut().unwrap().remove("radius");
        defaults.as_object_mut().unwrap().remove("casts_shadow");
        let defaults = local_lights_from_json(&json!({"point_lights":[defaults]})).unwrap();
        assert!(!defaults[0].light.casts_shadow);
        assert_eq!(
            defaults[0].light.shape,
            LightShape::Point {
                radius: LightShape::DEFAULT_RADIUS
            }
        );
    }

    #[test]
    fn authored_fixture_preserves_original_camera_light_recipe() {
        let fixture = fixture();
        for ((name, eye, target), spec) in
            CAMERAS.iter().zip(fixture["cameras"].as_array().unwrap())
        {
            let original = frame(name, *eye, *target, [1280, 720], 0., false);
            let from_fixture = frame_from_json(spec, [1280, 720], 0., None).unwrap();
            assert_eq!(from_fixture.camera.eye, original.camera.eye);
            assert_eq!(from_fixture.camera.view, original.camera.view);
            assert_eq!(from_fixture.camera.projection, original.camera.projection);
            assert_eq!(
                from_fixture.directional_lights[0],
                original.directional_lights[0]
            );
            assert_eq!(
                from_fixture.diffuse_environment,
                original.diffuse_environment
            );
            assert_eq!(
                from_fixture.reflection_environment,
                original.reflection_environment
            );
            assert_eq!(from_fixture.atmosphere, original.atmosphere);
        }
        let overridden =
            frame_from_json(&fixture["cameras"][0], [1280, 720], 0.6, Some(4)).unwrap();
        let applied = frame_metadata(&overridden);
        assert_eq!(fixture["cameras"][0]["sun"]["shadow_cascades"], 3);
        assert_eq!(applied["directional_lights"][0]["shadow"]["cascades"], 4);
        assert!((applied["exposure_stops"].as_f64().unwrap() - 0.6).abs() < 1e-6);
    }

    #[test]
    fn authored_recipes_resolve_in_the_shared_native_browser_harness() {
        for source in [
            include_str!("../../fixtures/sponza-daylight.json"),
            include_str!("../../fixtures/sponza-gi.json"),
            include_str!("../../fixtures/sponza-sunlit.json"),
            include_str!("../../fixtures/sponza-lamps.json"),
            include_str!("../../fixtures/bistro-noir.json"),
            include_str!("../../fixtures/bistro-daylight.json"),
            include_str!("../../fixtures/bistro-quality.json"),
        ] {
            let recipe: Value = serde_json::from_str(source).unwrap();
            resolve_settings(settings(false, false), Some(&recipe), None, None).unwrap();
            local_exposure_from_json(&recipe).unwrap();
            local_lights_from_json(&recipe).unwrap();
            fog_volumes_from_json(&recipe).unwrap();
            decals_from_json(&recipe).unwrap();
            sky_from_json(&recipe).unwrap();
            specular_probe_placements(&recipe).unwrap();
            let cameras = selected_cameras(Some(&recipe), "all").unwrap();
            assert!(!cameras.is_empty());
            let default = recipe["default_camera"].as_str().unwrap();
            assert_eq!(selected_cameras(Some(&recipe), default).unwrap().len(), 1);
            for camera in cameras {
                let frame = frame_from_json(
                    &camera,
                    [1920, 1080],
                    recipe["exposure_stops"].as_f64().unwrap() as f32,
                    None,
                )
                .unwrap();
                assert!(frame.camera.view.is_finite());
                assert!(frame.camera.projection.is_finite());
            }
            if let Some(volume) = recipe.get("dynamic_gi_volume") {
                volume_from_json(volume).unwrap();
            }
        }
    }

    #[test]
    fn sponza_sunlit_recipes_cover_the_source_building_not_just_the_courtyard() {
        // Independent geometry bounds from the pinned Intel base + curtains
        // export, including the far wood door at x=19.37..19.62 and roof at
        // y=18.76. Covered receivers must not sit in the volume's sky-fallback
        // fade merely because the visible courtyard itself fits the lattice.
        let source_min = Vec3::new(-16.2456, -1.1311, -9.4021);
        let source_max = Vec3::new(20.0013, 18.7572, 14.3639);
        for source in [
            include_str!("../../fixtures/sponza-sunlit.json"),
            include_str!("../../fixtures/sponza-lamps.json"),
        ] {
            let recipe: Value = serde_json::from_str(source).unwrap();
            let volume = volume_from_json(&recipe["dynamic_gi_volume"]).unwrap();
            let end = volume.origin
                + volume.spacing * Vec3::from_array(volume.probes.map(|n| (n - 1) as f32));
            assert!(
                volume.origin.cmple(source_min).all(),
                "first probe must enclose source minimum"
            );
            assert!(
                end.cmpge(source_max).all(),
                "last probe must enclose source maximum"
            );
        }
    }

    #[test]
    fn sponza_gallery_reflection_keeps_the_far_door_inside_its_full_influence() {
        let descriptor: Value =
            serde_json::from_str(include_str!("../../fixtures/sponza-gallery-probe.json")).unwrap();
        let (_, bytes) = probe_shape(&descriptor).unwrap();
        let probe = probe_from_json(&descriptor, &vec![0; bytes]).unwrap();
        // Source wood_door bounds, not the approximate gallery proxy box.
        let door_min = Vec3::new(19.3685, -0.6519, -1.2158);
        let door_max = Vec3::new(19.6152, 2.2091, 1.2791);
        let blend = probe.blend;
        assert!((probe.influence.min + blend).cmple(door_min).all());
        assert!((probe.influence.max - blend).cmpge(door_max).all());
        let proxy = probe.proxy.unwrap();
        assert!(probe.center.cmpge(proxy.min).all());
        assert!(probe.center.cmple(proxy.max).all());
    }

    #[test]
    fn authored_probe_checks_version_shape_and_exact_payload_before_expansion() {
        let mut spec = json!({"schema":1,"encoding":"rgba16f-le","levels":7,"face_size":64,
            "center":[0.,1.,0.],"influence":{"min":[-2.,0.,-2.],"max":[2.,3.,2.]},"blend":[1.,1.,1.]});
        let (_, bytes) = probe_shape(&spec).unwrap();
        assert_eq!(bytes, 262128);
        assert!(probe_from_json(&spec, &[0; 2]).is_err());
        assert!(probe_from_json(&spec, &vec![0; bytes]).is_ok());
        spec["schema"] = json!(2);
        assert!(probe_shape(&spec).is_err());
        spec["schema"] = json!(1);
        spec["face_size"] = json!(512);
        assert!(probe_shape(&spec).is_err());
    }

    #[test]
    fn second_fixture_keeps_its_named_camera_and_authored_scale() {
        let material_fixture: Value =
            serde_json::from_str(include_str!("../../fixtures/metal-rough-spheres.json")).unwrap();
        let cameras = selected_cameras(Some(&material_fixture), "materials").unwrap();
        assert_eq!(cameras.len(), 1);
        assert_eq!(cameras[0]["eye"], json!([3, 3, 11]));
        assert_eq!(model_scale(&material_fixture).unwrap(), 1000.);
        assert!(selected_cameras(Some(&material_fixture), "street").is_err());
        assert!(model_scale(&json!({"scale":0})).is_err());
        assert!(model_scale(&json!({"scale":-1})).is_err());
    }

    #[test]
    fn fixture_settings_are_resolved_before_explicit_overrides() {
        let mut requested = settings(false, false);
        requested.antialiasing = Antialiasing::Fsr2;
        requested.hardware_ray_tracing = true;
        let fixture = json!({"settings":requested});
        let resolved =
            resolve_settings(settings(false, false), Some(&fixture), None, None).unwrap();
        assert_eq!(resolved.antialiasing, Antialiasing::Fsr2);
        assert!(resolved.hardware_ray_tracing);
        let override_settings = json!(settings(false, false));
        let resolved = resolve_settings(
            settings(false, false),
            Some(&fixture),
            Some(&override_settings),
            Some(Antialiasing::Smaa),
        )
        .unwrap();
        assert_eq!(resolved.antialiasing, Antialiasing::Smaa);
        assert!(!resolved.hardware_ray_tracing);
    }

    #[test]
    fn caller_owned_images_share_one_allowance_before_expansion() {
        for (format, name, companion) in [
            (ktx2::Format::BC7_UNORM_BLOCK, "bc7", false),
            (ktx2::Format::ASTC_4x4_UNORM_BLOCK, "astc4x4", true),
        ] {
            let file = ktx2_writer::ktx2(format, [4, 4], &[vec![0; 16]], false);
            let mut png = std::io::Cursor::new(Vec::new());
            image::DynamicImage::ImageRgba8(image::RgbaImage::new(4, 4))
                .write_to(&mut png, image::ImageFormat::Png)
                .unwrap();
            let companion = companion.then_some(png.get_ref().as_slice());
            let per_image = if companion.is_some() { 80 } else { 16 };
            let mut allowance = SuppliedImages::new(
                asset::LoadBudget::new(1024, per_image + per_image / 2).unwrap(),
            );
            let manifest = json!({"schema":1,"format":name});
            let entry = json!({"width":4,"height":4});
            assert!(
                allowance
                    .decode(&manifest, &entry, &file, companion)
                    .is_ok()
            );
            let error = allowance
                .decode(&manifest, &entry, &file, companion)
                .unwrap_err();
            assert!(error.contains("before expansion"), "{error}");
            assert_eq!(allowance.used, per_image);
        }
    }

    #[cfg(unix)]
    #[test]
    fn auxiliary_assets_refuse_parent_absolute_and_symlink_escapes() {
        let root = std::env::temp_dir().join(format!(
            "sgl-bistro-path-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("assets/Compressed")).unwrap();
        std::fs::create_dir_all(root.join("outside")).unwrap();
        std::fs::write(root.join("outside/private.png"), b"outside fixture").unwrap();
        std::fs::write(root.join("assets/valid.png"), b"inside fixture").unwrap();
        std::fs::write(root.join("assets/Compressed/valid.ktx2"), b"inside fixture").unwrap();
        let assets = root.join("assets");
        assert!(asset_path(&assets, "valid.png").is_ok());
        assert!(asset_path(&assets, "Compressed/valid.ktx2").is_ok());
        assert!(asset_path(&assets, "../outside/private.png").is_err());
        assert!(asset_path(&assets, root.join("outside/private.png").to_str().unwrap()).is_err());
        std::os::unix::fs::symlink(root.join("outside/private.png"), assets.join("escape.png"))
            .unwrap();
        assert!(asset_path(&assets, "escape.png").is_err());
        std::fs::remove_dir_all(assets.join("Compressed")).unwrap();
        std::os::unix::fs::symlink(root.join("outside"), assets.join("Compressed")).unwrap();
        assert!(asset_path(&assets, "Compressed/private.png").is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
