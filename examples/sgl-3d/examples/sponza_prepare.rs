//! Explicit local preparation of Intel Sponza and its Colorful Curtains add-on.
//! No renderer policy, geometry changes, or automatic bake-staleness checks.
use image::{ImageBuffer, Rgba, Rgba32FImage, imageops::FilterType};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    error::Error,
    path::{Path, PathBuf},
};
type Result<T> = std::result::Result<T, Box<dyn Error>>;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    Color,
    Normal,
    Data,
}
fn srgb_to_linear(value: f32) -> f32 {
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}
fn linear_to_srgb(value: f32) -> f32 {
    if value <= 0.0031308 {
        value * 12.92
    } else {
        1.055 * value.powf(1. / 2.4) - 0.055
    }
}

fn convert(input: &image::RgbaImage, role: Role, maximum: u32) -> image::RgbaImage {
    let mut linear: Rgba32FImage = ImageBuffer::from_fn(input.width(), input.height(), |x, y| {
        let mut pixel = input.get_pixel(x, y).0.map(|v| f32::from(v) / 255.);
        if role == Role::Color {
            for channel in 0..3 {
                pixel[channel] = srgb_to_linear(pixel[channel]) * pixel[3];
            }
        }
        Rgba(pixel)
    });
    let scale = (maximum as f32 / input.width().max(input.height()) as f32).min(1.);
    if scale < 1. {
        linear = image::imageops::resize(
            &linear,
            (input.width() as f32 * scale).round().max(1.) as u32,
            (input.height() as f32 * scale).round().max(1.) as u32,
            FilterType::Lanczos3,
        );
    }
    ImageBuffer::from_fn(linear.width(), linear.height(), |x, y| {
        let mut pixel = linear.get_pixel(x, y).0;
        if role == Role::Color {
            for channel in 0..3 {
                pixel[channel] = linear_to_srgb(pixel[channel].max(0.) / pixel[3].max(1e-8));
            }
        } else if role == Role::Normal {
            // glTF tangent-space XYZ is linear and +Y-up. Renormalize after
            // filtering without channel reconstruction or inversion.
            let mut normal =
                glam::Vec3::new(pixel[0] * 2. - 1., pixel[1] * 2. - 1., pixel[2] * 2. - 1.);
            normal = normal.try_normalize().unwrap_or(glam::Vec3::Z);
            pixel[..3].copy_from_slice(&(normal * 0.5 + glam::Vec3::splat(0.5)).to_array());
        }
        Rgba(pixel.map(|v| (v.clamp(0., 1.) * 255.).round() as u8))
    })
}

fn resolved_destination(path: &Path) -> std::io::Result<PathBuf> {
    match std::fs::canonicalize(path) {
        Ok(path) => Ok(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
                return Err(std::io::Error::other("dangling output symlink"));
            }
            let path = std::path::absolute(path)?;
            let name = path.file_name().ok_or_else(|| {
                std::io::Error::other("output path must have a resolvable directory name")
            })?;
            Ok(resolved_destination(path.parent().unwrap())?.join(name))
        }
        Err(error) => Err(error),
    }
}

fn same_file(left: &Path, right: &Path) -> std::io::Result<bool> {
    let left = std::fs::metadata(left)?;
    let right = std::fs::metadata(right)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(left.dev() == right.dev() && left.ino() == right.ino())
    }
    #[cfg(not(unix))]
    {
        // Canonical paths below cover directory/file symlinks on all targets;
        // Unix additionally exposes stable identities for hardlink aliases.
        let _ = (left, right);
        Ok(false)
    }
}

// Only local relative resources are accepted. Decode percent escapes before
// checking traversal; canonicalization also rejects symlinks escaping the source.
fn resource(root: &Path, uri: &str) -> Result<PathBuf> {
    let mut bytes = Vec::new();
    let mut chars = uri.as_bytes().iter().copied();
    while let Some(c) = chars.next() {
        bytes.push(if c == b'%' {
            let a = chars.next().ok_or("truncated URI escape")?;
            let b = chars.next().ok_or("truncated URI escape")?;
            u8::from_str_radix(std::str::from_utf8(&[a, b])?, 16)?
        } else {
            c
        });
    }
    let decoded = String::from_utf8(bytes)?;
    let path = Path::new(&decoded);
    if decoded.contains(['\\', ':', '?', '#', '\0'])
        || path.is_absolute()
        || path.components().any(|c| {
            !matches!(
                c,
                std::path::Component::Normal(_) | std::path::Component::CurDir
            )
        })
    {
        return Err(format!("unsafe resource URI: {uri}").into());
    }
    let root = root.canonicalize()?;
    let path = root.join(path).canonicalize()?;
    if !path.starts_with(root) {
        return Err("resource escapes source directory".into());
    }
    Ok(path)
}

fn array<'a>(doc: &'a Value, key: &str) -> &'a [Value] {
    doc[key].as_array().map(Vec::as_slice).unwrap_or(&[])
}

fn validate_extensions(value: &Value, location: &str) -> Result<()> {
    match value {
        Value::Object(object) => {
            for (key, child) in object {
                if key == "extras" {
                    continue;
                }
                if key == "extensions" {
                    for extension in child
                        .as_object()
                        .ok_or("extensions must be an object")?
                        .keys()
                    {
                        if extension != "KHR_lights_punctual"
                            || !(location.is_empty() || location.starts_with("/nodes/"))
                        {
                            return Err(
                                format!("unsupported extension {extension} at {location}").into()
                            );
                        }
                    }
                }
                validate_extensions(child, &format!("{location}/{key}"))?;
            }
        }
        Value::Array(values) => {
            for (i, child) in values.iter().enumerate() {
                validate_extensions(child, &format!("{location}/{i}"))?;
            }
        }
        _ => {}
    }
    Ok(())
}

struct Source {
    root: PathBuf,
    document: Value,
    inputs: Vec<PathBuf>,
}
impl Source {
    fn read(path: &Path) -> Result<Self> {
        let path = path.canonicalize()?;
        let root = path.parent().ok_or("source needs a parent")?.to_owned();
        let document: Value = serde_json::from_slice(&std::fs::read(&path)?)?;
        if document["asset"]["version"] != "2.0" {
            return Err("expected glTF 2.0".into());
        }
        validate_extensions(&document, "")?;
        for key in ["extensionsUsed", "extensionsRequired"] {
            for extension in array(&document, key) {
                if extension != "KHR_lights_punctual" {
                    return Err(format!("unsupported extension {extension}").into());
                }
            }
        }
        for key in ["skins", "animations"] {
            if !array(&document, key).is_empty() {
                return Err(format!("{key} unsupported in this static preparation").into());
            }
        }
        if array(&document, "scenes").len() != 1 || document["scene"].as_u64().unwrap_or(0) != 0 {
            return Err("expected one source scene".into());
        }
        let mut inputs = vec![path];
        for key in ["buffers", "images"] {
            for item in array(&document, key) {
                inputs.push(resource(
                    &root,
                    item["uri"]
                        .as_str()
                        .ok_or("expected external resource URI")?,
                )?);
            }
        }
        Ok(Self {
            root,
            document,
            inputs,
        })
    }
}

fn validate_outputs(sources: &[Source], output: &Path, names: &[String]) -> Result<()> {
    let output = resolved_destination(output)?;
    for source in sources {
        if output.starts_with(&source.root) || source.root.starts_with(&output) {
            return Err("output must be separate from source directories".into());
        }
    }
    for name in names {
        let target = output.join(name);
        let resolved = resolved_destination(&target)?;
        if !resolved.starts_with(&output) {
            return Err("output resource escapes output directory".into());
        }
        for source in sources {
            if resolved.starts_with(&source.root) {
                return Err("output resource aliases source directory".into());
            }
            if target.exists() {
                for input in &source.inputs {
                    if same_file(input, &target)? || input == &resolved {
                        return Err("output resource aliases input file".into());
                    }
                }
            }
        }
    }
    Ok(())
}

fn roles(doc: &Value) -> Result<BTreeMap<usize, Role>> {
    let mut roles = BTreeMap::new();
    for material in array(doc, "materials") {
        for (slot, role) in [
            (
                &material["pbrMetallicRoughness"]["baseColorTexture"],
                Role::Color,
            ),
            (&material["emissiveTexture"], Role::Color),
            (&material["normalTexture"], Role::Normal),
            (
                &material["pbrMetallicRoughness"]["metallicRoughnessTexture"],
                Role::Data,
            ),
            (&material["occlusionTexture"], Role::Data),
        ] {
            if slot.is_null() {
                continue;
            }
            let texture = slot["index"].as_u64().ok_or("invalid texture index")? as usize;
            let image = doc["textures"][texture]["source"]
                .as_u64()
                .ok_or("invalid texture source")? as usize;
            if image >= array(doc, "images").len() {
                return Err("image index out of bounds".into());
            }
            if let Some(previous) = roles.insert(image, role)
                && previous != role
            {
                return Err("image used for incompatible color, normal or data roles".into());
            }
        }
    }
    Ok(roles)
}

fn index(value: &mut Value, offset: usize, count: usize) -> Result<()> {
    let old = value.as_u64().ok_or("index must be an unsigned integer")? as usize;
    if old >= count {
        return Err(format!("index {old} outside array of {count}").into());
    }
    *value = json!(old + offset);
    Ok(())
}
fn field(value: &mut Value, key: &str, offset: usize, count: usize) -> Result<()> {
    if let Some(value) = value.get_mut(key) {
        index(value, offset, count)?;
    }
    Ok(())
}
const ARRAYS: [&str; 9] = [
    "bufferViews",
    "accessors",
    "images",
    "samplers",
    "textures",
    "materials",
    "meshes",
    "cameras",
    "nodes",
];

// Merge static core indices without touching geometry bytes or authored TRS.
// Punctual lights are the sole extension in Intel's source and are preserved.
fn append(
    destination: &mut Value,
    source: &Source,
    binary: &mut Vec<u8>,
    image_prefix: usize,
) -> Result<()> {
    let mut doc = source.document.clone();
    let offsets: BTreeMap<_, _> = ARRAYS
        .iter()
        .map(|&k| (k, array(destination, k).len()))
        .collect();
    let counts: BTreeMap<_, _> = ARRAYS.iter().map(|&k| (k, array(&doc, k).len())).collect();
    let mut buffers = Vec::new();
    for buffer in array(&doc, "buffers") {
        while !binary.len().is_multiple_of(4) {
            binary.push(0);
        }
        let bytes = std::fs::read(resource(
            &source.root,
            buffer["uri"].as_str().ok_or("buffer URI missing")?,
        )?)?;
        let length = buffer["byteLength"]
            .as_u64()
            .ok_or("buffer length missing")? as usize;
        if bytes.len() != length {
            return Err("buffer byteLength differs from resource length".into());
        }
        buffers.push((binary.len(), length));
        binary.extend(bytes);
    }
    if let Some(views) = doc["bufferViews"].as_array_mut() {
        for view in views {
            let b = view["buffer"].as_u64().ok_or("buffer index missing")? as usize;
            let &(offset, length) = buffers.get(b).ok_or("buffer index out of bounds")?;
            let start = view["byteOffset"].as_u64().unwrap_or(0) as usize;
            let size = view["byteLength"].as_u64().ok_or("view length missing")? as usize;
            if start.checked_add(size).is_none_or(|end| end > length) {
                return Err("bufferView outside buffer".into());
            }
            view["buffer"] = json!(0);
            view["byteOffset"] = json!(offset + start);
        }
    }
    let remap =
        |v: &mut Value, key: &str, array: &str| field(v, key, offsets[array], counts[array]);
    if let Some(accessors) = doc["accessors"].as_array_mut() {
        for a in accessors {
            remap(a, "bufferView", "bufferViews")?;
            if let Some(sparse) = a.get_mut("sparse") {
                for kind in ["indices", "values"] {
                    remap(&mut sparse[kind], "bufferView", "bufferViews")?;
                }
            }
        }
    }
    if let Some(images) = doc["images"].as_array_mut() {
        for (i, image) in images.iter_mut().enumerate() {
            image["uri"] = json!(format!("textures/{:03}.png", image_prefix + i));
            image["mimeType"] = json!("image/png");
        }
    }
    if let Some(textures) = doc["textures"].as_array_mut() {
        for texture in textures {
            remap(texture, "source", "images")?;
            remap(texture, "sampler", "samplers")?;
        }
    }
    if let Some(materials) = doc["materials"].as_array_mut() {
        for m in materials {
            for slot in ["normalTexture", "occlusionTexture", "emissiveTexture"] {
                if let Some(t) = m.get_mut(slot) {
                    remap(t, "index", "textures")?;
                }
            }
            if let Some(pbr) = m.get_mut("pbrMetallicRoughness") {
                for slot in ["baseColorTexture", "metallicRoughnessTexture"] {
                    if let Some(t) = pbr.get_mut(slot) {
                        remap(t, "index", "textures")?;
                    }
                }
            }
        }
    }
    if let Some(meshes) = doc["meshes"].as_array_mut() {
        for mesh in meshes {
            for primitive in mesh["primitives"]
                .as_array_mut()
                .ok_or("mesh missing primitives")?
            {
                remap(primitive, "indices", "accessors")?;
                remap(primitive, "material", "materials")?;
                for value in primitive["attributes"]
                    .as_object_mut()
                    .ok_or("primitive missing attributes")?
                    .values_mut()
                {
                    index(value, offsets["accessors"], counts["accessors"])?;
                }
                if let Some(targets) = primitive["targets"].as_array_mut() {
                    for target in targets {
                        for value in target
                            .as_object_mut()
                            .ok_or("invalid morph target")?
                            .values_mut()
                        {
                            index(value, offsets["accessors"], counts["accessors"])?;
                        }
                    }
                }
            }
        }
    }
    let light_offset = array(&destination["extensions"]["KHR_lights_punctual"], "lights").len();
    let light_count = array(&doc["extensions"]["KHR_lights_punctual"], "lights").len();
    if let Some(nodes) = doc["nodes"].as_array_mut() {
        for node in nodes {
            remap(node, "mesh", "meshes")?;
            remap(node, "camera", "cameras")?;
            if node.get("skin").is_some() {
                return Err("skin reference in static source".into());
            }
            if let Some(children) = node["children"].as_array_mut() {
                for child in children {
                    index(child, offsets["nodes"], counts["nodes"])?;
                }
            }
            if let Some(light) = node.pointer_mut("/extensions/KHR_lights_punctual/light") {
                index(light, light_offset, light_count)?;
            }
        }
    }
    if light_count > 0 {
        if destination.get("extensions").is_none() {
            destination["extensions"] = json!({"KHR_lights_punctual":{"lights":[]}});
        }
        destination["extensions"]["KHR_lights_punctual"]["lights"]
            .as_array_mut()
            .unwrap()
            .extend(
                array(&doc["extensions"]["KHR_lights_punctual"], "lights")
                    .iter()
                    .cloned(),
            );
        destination["extensionsUsed"] = json!(["KHR_lights_punctual"]);
    }
    if array(&doc, "extensionsRequired").contains(&json!("KHR_lights_punctual")) {
        destination["extensionsRequired"] = json!(["KHR_lights_punctual"]);
    }
    let mut roots = doc["scenes"][0]["nodes"]
        .as_array()
        .ok_or("scene roots missing")?
        .clone();
    for root in &mut roots {
        index(root, offsets["nodes"], counts["nodes"])?;
    }
    destination["scenes"][0]["nodes"]
        .as_array_mut()
        .unwrap()
        .extend(roots);
    for key in ARRAYS {
        destination[key]
            .as_array_mut()
            .unwrap()
            .extend(array(&doc, key).iter().cloned());
    }
    destination["extras"]["sourceAssets"]
        .as_array_mut()
        .unwrap()
        .push(doc["asset"].clone());
    Ok(())
}

fn neutral_environment(output: &Path) -> Result<()> {
    let value = (linear_to_srgb(0.125) * 255.).round() as u8;
    image::RgbImage::from_pixel(4, 2, image::Rgb([value; 3])).save(output.join("panorama.png"))?;
    // IEEE 754 binary16: 0.125 = 0x3000 and 1.0 = 0x3c00.
    // A uniform radiance field is unchanged by normalized PMREM convolution.
    let texel: Vec<u8> = [0x3000_u16, 0x3000, 0x3000, 0x3c00]
        .into_iter()
        .flat_map(u16::to_le_bytes)
        .collect();
    std::fs::write(output.join("pmrem.rgba16"), texel.repeat(336 * 64))?;
    std::fs::write(
        output.join("environment.json"),
        serde_json::to_vec_pretty(&json!({
            "width":336,"height":64,"generator":"SGL authored uniform neutral fill",
            "linear_radiance":[0.125,0.125,0.125],
            "description":"Authored neutral fill, not calibrated sunlight. Constant radiance is invariant under normalized convolution; no GPU or HDR bake. Panorama is sRGB8 quantized; atlas is exact linear RGBA16F."
        }))?,
    )?;
    Ok(())
}

fn prepare(sources: &[Source], output: &Path, maximum: u32) -> Result<Value> {
    let image_count: usize = sources
        .iter()
        .map(|s| array(&s.document, "images").len())
        .sum();
    let mut names: Vec<String> = [
        "Sponza.gltf",
        "Sponza.bin",
        "environment.json",
        "panorama.png",
        "pmrem.rgba16",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    names.extend((0..image_count).map(|i| format!("textures/{i:03}.png")));
    validate_outputs(sources, output, &names)?;
    let image_roles: Vec<_> = sources
        .iter()
        .map(|s| roles(&s.document))
        .collect::<Result<_>>()?;
    let mut doc = json!({"asset":{"version":"2.0","generator":"SGL explicit Intel Sponza preparation"},
        "scene":0,"scenes":[{"name":"Intel Sponza with Colorful Curtains","nodes":[]}],"extras":{"sourceAssets":[]}});
    for key in ARRAYS {
        doc[key] = json!([]);
    }
    let mut binary = Vec::new();
    let mut image_offset = 0;
    for source in sources {
        append(&mut doc, source, &mut binary, image_offset)?;
        image_offset += array(&source.document, "images").len();
    }
    doc["buffers"] = json!([{"uri":"Sponza.bin","byteLength":binary.len()}]);
    std::fs::create_dir_all(output.join("textures"))?;
    image_offset = 0;
    for (source, roles) in sources.iter().zip(image_roles) {
        for (i, image) in array(&source.document, "images").iter().enumerate() {
            let path = resource(
                &source.root,
                image["uri"].as_str().ok_or("image URI missing")?,
            )?;
            let input = image::open(path)?.into_rgba8();
            convert(
                &input,
                roles.get(&i).copied().unwrap_or(Role::Data),
                maximum,
            )
            .save(output.join(format!("textures/{:03}.png", image_offset + i)))?;
            println!("Sponza textures: {}/{image_count}", image_offset + i + 1);
        }
        image_offset += array(&source.document, "images").len();
    }
    std::fs::write(output.join("Sponza.bin"), binary)?;
    neutral_environment(output)?;
    std::fs::write(output.join("Sponza.gltf"), serde_json::to_vec(&doc)?)?;
    Ok(doc)
}

fn main() -> Result<()> {
    let mut base = None;
    let mut curtains = None;
    let mut output = None;
    let mut maximum = 512;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let value = args.next().ok_or_else(|| format!("{arg} needs a value"))?;
        match arg.as_str() {
            "--base" => base = Some(PathBuf::from(value)),
            "--curtains" => curtains = Some(PathBuf::from(value)),
            "--output" => output = Some(PathBuf::from(value)),
            "--max-texture" => maximum = value.parse()?,
            _ => return Err(format!("unknown option {arg}").into()),
        }
    }
    if !(1..=16384).contains(&maximum) {
        return Err("--max-texture must be 1..16384".into());
    }
    let sources = [
        Source::read(&base.ok_or("--base required")?)?,
        Source::read(&curtains.ok_or("--curtains required")?)?,
    ];
    let output = output.ok_or("--output required")?;
    let doc = prepare(&sources, &output, maximum)?;
    println!(
        "Prepared {} ({} nodes, {} meshes)",
        output.join("Sponza.gltf").display(),
        array(&doc, "nodes").len(),
        array(&doc, "meshes").len()
    );
    Ok(())
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    fn temporary(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("sgl-sponza-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        path
    }
    fn triangle(root: &Path, x: f32) -> Source {
        std::fs::create_dir_all(root).unwrap();
        let mut bytes = vec![0, 0, 1, 0, 2, 0, 0, 0];
        let positions = [x, 0., 0., x + 1., 0., 0., x, 1., 0.];
        let normals = [0_f32, 0., 1., 0., 0., 1., 0., 0., 1.];
        bytes.extend_from_slice(bytemuck::cast_slice(&positions));
        bytes.extend_from_slice(bytemuck::cast_slice(&normals));
        bytes.extend_from_slice(bytemuck::cast_slice(&[0_f32, 0., 1., 0., 0., 1.]));
        std::fs::write(root.join("mesh.bin"), bytes).unwrap();
        image::RgbaImage::from_pixel(1, 1, Rgba([64, 128, 192, 255]))
            .save(root.join("color.png"))
            .unwrap();
        let doc = json!({"asset":{"version":"2.0"},"scene":0,
            "scenes":[{"nodes":[0]}],"nodes":[{"children":[1],"translation":[10,0,0]}, {"mesh":0,"translation":[0,2,0],"scale":[2,2,2],"camera":0,"extensions":{"KHR_lights_punctual":{"light":0}}}],
            "extensionsUsed":["KHR_lights_punctual"],"extensions":{"KHR_lights_punctual":{"lights":[{"type":"point","intensity":0}]}},
            "cameras":[{"type":"perspective","perspective":{"yfov":1,"znear":0.1}}],
            "buffers":[{"uri":"mesh.bin","byteLength":104}],
            "bufferViews":[{"buffer":0,"byteLength":6},{"buffer":0,"byteOffset":8,"byteLength":36},{"buffer":0,"byteOffset":44,"byteLength":36},{"buffer":0,"byteOffset":80,"byteLength":24}],
            "accessors":[{"bufferView":0,"componentType":5123,"count":3,"type":"SCALAR"},
              {"bufferView":1,"componentType":5126,"count":3,"type":"VEC3","min":[x,0,0],"max":[x+1.,1,0]},
              {"bufferView":2,"componentType":5126,"count":3,"type":"VEC3"},{"bufferView":3,"componentType":5126,"count":3,"type":"VEC2"}],
            "images":[{"uri":"color.png"}],"samplers":[{}],"textures":[{"source":0,"sampler":0}],
            "materials":[{"name":format!("material-{x}"),"pbrMetallicRoughness":{"baseColorTexture":{"index":0},"metallicFactor":0.25,"roughnessFactor":0.75},"doubleSided":true}],
            "meshes":[{"primitives":[{"indices":0,"attributes":{"POSITION":1,"NORMAL":2,"TEXCOORD_0":3},"material":0}]}]});
        std::fs::write(root.join("scene.gltf"), serde_json::to_vec(&doc).unwrap()).unwrap();
        Source::read(&root.join("scene.gltf")).unwrap()
    }
    #[test]
    fn merged_bytes_indices_transforms_and_materials_load_through_real_loader() {
        let root = temporary("merge");
        let sources = [
            triangle(&root.join("base"), 0.),
            triangle(&root.join("curtains"), 3.),
        ];
        let output = root.join("prepared");
        let doc = prepare(&sources, &output, 512).unwrap();
        assert_eq!(doc["scenes"][0]["nodes"], json!([0, 2]));
        assert_eq!(doc["nodes"][2]["children"], json!([3]));
        assert_eq!(doc["nodes"][3]["camera"], 1);
        assert_eq!(
            doc["nodes"][3]["extensions"]["KHR_lights_punctual"]["light"],
            1
        );
        assert_eq!(doc["textures"][1], json!({"source":1,"sampler":1}));
        assert_eq!(
            doc["materials"][1]["pbrMetallicRoughness"]["baseColorTexture"]["index"],
            1
        );
        assert_eq!(doc["bufferViews"][5]["byteOffset"], 112);
        let bytes = std::fs::read(output.join("Sponza.bin")).unwrap();
        assert_eq!(
            &bytes[..104],
            std::fs::read(root.join("base/mesh.bin")).unwrap()
        );
        assert_eq!(
            &bytes[104..],
            std::fs::read(root.join("curtains/mesh.bin")).unwrap()
        );
        let loaded = sgl_3d::asset::load(&output.join("Sponza.gltf")).unwrap();
        assert_eq!(loaded.meshes.len(), 2);
        let mut points: Vec<_> = loaded
            .meshes
            .iter()
            .flat_map(|m| m.vertices.iter().map(|v| v.position))
            .collect();
        points.sort_by(|a, b| a[0].total_cmp(&b[0]).then(a[1].total_cmp(&b[1])));
        assert_eq!(
            points,
            vec![
                [10., 2., 0.],
                [10., 4., 0.],
                [12., 2., 0.],
                [16., 2., 0.],
                [16., 4., 0.],
                [18., 2., 0.]
            ]
        );
        for mesh in &loaded.meshes {
            assert_eq!(mesh.indices, vec![0, 1, 2]);
        }
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn filtering_preserves_gltf_normal_y_and_uses_linear_premultiplied_color() {
        let tilted = image::RgbaImage::from_pixel(2, 1, Rgba([128, 37, 218, 255]));
        let normal = convert(&tilted, Role::Normal, 1).get_pixel(0, 0).0;
        let v = glam::Vec3::from_array(
            [normal[0], normal[1], normal[2]].map(|x| f32::from(x) / 127.5 - 1.),
        );
        assert!(v.y < -0.69 && v.z > 0.69 && (v.length() - 1.).abs() < 0.01);
        let pair = image::RgbaImage::from_fn(2, 1, |x, _| {
            Rgba(if x == 0 {
                [0, 0, 0, 255]
            } else {
                [255, 255, 255, 255]
            })
        });
        assert!((187..=189).contains(&convert(&pair, Role::Color, 1).get_pixel(0, 0)[0]));
        assert!((127..=129).contains(&convert(&pair, Role::Data, 1).get_pixel(0, 0)[0]));
        let alpha = image::RgbaImage::from_fn(2, 1, |x, _| {
            Rgba(if x == 0 {
                [255, 0, 0, 255]
            } else {
                [0, 0, 255, 0]
            })
        });
        let filtered = convert(&alpha, Role::Color, 1).get_pixel(0, 0).0;
        assert_eq!(&filtered[..3], &[255, 0, 0]);
        assert!((127..=129).contains(&filtered[3]));
        let color = image::RgbaImage::from_pixel(1, 1, Rgba([24, 146, 221, 179]));
        assert_eq!(convert(&color, Role::Color, 512), color);
    }
    #[test]
    fn unsafe_resource_and_output_aliases_are_rejected_before_writes() {
        let root = temporary("paths");
        let sources = [triangle(&root.join("source"), 0.)];
        let input = &sources[0].root;
        for uri in [
            "../mesh.bin",
            "%2e%2e/mesh.bin",
            "/mesh.bin",
            "https://example.org/a",
            "..\\mesh.bin",
        ] {
            assert!(resource(input, uri).is_err());
        }
        assert!(prepare(&sources, input, 1).is_err());
        assert!(prepare(&sources, &root, 1).is_err());
        #[cfg(unix)]
        {
            let output = root.join("output");
            std::fs::create_dir_all(&output).unwrap();
            std::fs::hard_link(input.join("mesh.bin"), output.join("Sponza.bin")).unwrap();
            assert!(
                prepare(&sources, &output, 1)
                    .unwrap_err()
                    .to_string()
                    .contains("aliases")
            );
            std::fs::remove_file(output.join("Sponza.bin")).unwrap();
            std::os::unix::fs::symlink(input, root.join("alias")).unwrap();
            assert!(prepare(&sources, &root.join("alias"), 1).is_err());
            std::os::unix::fs::symlink(root.join("not-created"), output.join("Sponza.bin"))
                .unwrap();
            assert!(prepare(&sources, &output, 1).is_err());
            assert!(!root.join("not-created").exists());
            std::fs::write(root.join("outside.bin"), [1]).unwrap();
            std::os::unix::fs::symlink(root.join("outside.bin"), input.join("escape.bin")).unwrap();
            assert!(resource(input, "escape.bin").is_err());
        }
        assert_eq!(
            std::fs::metadata(input.join("mesh.bin")).unwrap().len(),
            104
        );
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn unknown_extensions_fail_instead_of_being_discarded() {
        assert!(
            validate_extensions(
                &json!({"materials":[{"extensions":{"KHR_materials_transmission":{}}}]}),
                ""
            )
            .is_err()
        );
        assert!(
            validate_extensions(
                &json!({"extras":{"extensions":{"application_note":{}}}}),
                ""
            )
            .is_ok()
        );
    }
    #[test]
    fn constant_environment_has_exact_half_float_radiance_and_valid_layout() {
        let output = temporary("environment");
        neutral_environment(&output).unwrap();
        let bytes = std::fs::read(output.join("pmrem.rgba16")).unwrap();
        assert_eq!(bytes.len(), 336 * 64 * 8);
        for texel in bytes.chunks_exact(8) {
            assert_eq!(texel, &[0, 48, 0, 48, 0, 48, 0, 60]);
        }
        let panorama = image::open(output.join("panorama.png"))
            .unwrap()
            .into_rgb8();
        assert_eq!(panorama.dimensions(), (4, 2));
        assert!(panorama.pixels().all(|p| p.0 == [99; 3]));
        std::fs::remove_dir_all(output).unwrap();
    }
}
