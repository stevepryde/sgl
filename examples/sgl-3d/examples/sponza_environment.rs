//! Explicit source-domain extraction for Sponza's CC0 Kloppenheim sky.
//! An authored sky approximation, not a renderer feature or hidden rebake.
use image::{Rgb, Rgb32FImage};
use serde_json::json;
use std::{error::Error, f64::consts::PI, fs, io::Write, path::Path};
type Result<T> = std::result::Result<T, Box<dyn Error>>;
const OUTPUT_FILES: [&str; 3] = [
    "sky-lighting.hdr",
    "sky-panorama.png",
    "SKY-PROCESSING.json",
];

fn check_output_paths(out: &Path) -> Result<()> {
    for name in OUTPUT_FILES {
        let path = out.join(name);
        match fs::symlink_metadata(&path) {
            Ok(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    format!(
                        "export destination already exists: {}; use a fresh work directory",
                        path.display()
                    ),
                )
                .into());
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}
fn direction(x: u32, y: u32, w: u32, h: u32) -> [f64; 3] {
    let theta = PI * (y as f64 + 0.5) / h as f64;
    let phi = 2. * PI * ((x as f64 + 0.5) / w as f64 - 0.5);
    [
        theta.sin() * phi.cos(),
        theta.cos(),
        theta.sin() * phi.sin(),
    ]
}
fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    (0..3).map(|i| a[i] * b[i]).sum()
}
fn omega(y: u32, w: u32, h: u32) -> f64 {
    2. * PI / w as f64 * ((PI * y as f64 / h as f64).cos() - (PI * (y + 1) as f64 / h as f64).cos())
}
fn luminance(p: Rgb<f32>) -> f64 {
    0.2126 * p[0] as f64 + 0.7152 * p[1] as f64 + 0.0722 * p[2] as f64
}
fn normalize(v: [f64; 3]) -> [f64; 3] {
    let length = dot(v, v).sqrt();
    v.map(|c| c / length)
}
// Integer box reduction in linear radiance, weighted by each source texel's
// exact spherical solid angle. This preserves the sphere-integrated RGB.
fn reduce(source: &Rgb32FImage, width: u32) -> Result<Rgb32FImage> {
    let (w, h) = source.dimensions();
    if w != h * 2 || width == 0 || !w.is_multiple_of(width) {
        return Err("expected 2:1 panorama and integer width reduction".into());
    }
    let factor = w / width;
    let mut output = Rgb32FImage::new(width, h / factor);
    for (x, y, pixel) in output.enumerate_pixels_mut() {
        let mut sum = [0.; 3];
        let mut weight = 0.;
        for sy in y * factor..(y + 1) * factor {
            let solid = omega(sy, w, h);
            for sx in x * factor..(x + 1) * factor {
                for (c, value) in sum.iter_mut().enumerate() {
                    *value += source.get_pixel(sx, sy)[c] as f64 * solid;
                }
                weight += solid;
            }
        }
        *pixel = Rgb(sum.map(|v| (v / weight) as f32));
    }
    Ok(output)
}
fn integrated(image: &Rgb32FImage, upward: bool) -> [f64; 3] {
    let (w, h) = image.dimensions();
    let mut sum = [0.; 3];
    for (x, y, p) in image.enumerate_pixels() {
        let weight = omega(y, w, h)
            * if upward {
                direction(x, y, w, h)[1].max(0.)
            } else {
                1.
            };
        for c in 0..3 {
            sum[c] += p[c] as f64 * weight;
        }
    }
    sum
}
fn extract(source: &Rgb32FImage) -> Result<(Rgb32FImage, serde_json::Value)> {
    let (w, h) = source.dimensions();
    if source
        .pixels()
        .any(|p| p.0.iter().any(|v| !v.is_finite() || *v < 0.))
    {
        return Err("invalid source radiance".into());
    }
    let (px, py, peak) = source
        .enumerate_pixels()
        .filter(|(_, y, _)| *y < h / 2)
        .max_by(|a, b| luminance(*a.2).total_cmp(&luminance(*b.2)))
        .ok_or("empty sky")?;
    let seed = direction(px, py, w, h);
    let mut ring = Vec::new();
    for (x, y, p) in source.enumerate_pixels() {
        let d = dot(direction(x, y, w, h), seed);
        if d <= 1_f64.to_radians().cos() && d > 2_f64.to_radians().cos() {
            ring.push(luminance(*p));
        }
    }
    if ring.is_empty() {
        return Err("no local sky ring".into());
    }
    ring.sort_by(f64::total_cmp);
    let rank = (ring.len() - 1) as f64 * 0.95;
    let lo = rank.floor() as usize;
    let hi = rank.ceil() as usize;
    let cap = ring[lo] + (ring[hi] - ring[lo]) * rank.fract();
    if cap <= 0. || luminance(*peak) <= cap {
        return Err("no extractable sun lobe".into());
    }
    let mut sky = source.clone();
    let mut centroid = [0.; 3];
    let mut count = 0;
    for (x, y, p) in sky.enumerate_pixels_mut() {
        let d = direction(x, y, w, h);
        let l = luminance(*p);
        if dot(d, seed) > 1_f64.to_radians().cos() && l > cap {
            let old = *p;
            *p = Rgb(p.0.map(|v| (v as f64 * cap / l) as f32));
            let removed = Rgb(std::array::from_fn(|c| old[c] - p[c]));
            let weight = luminance(removed) * omega(y, w, h);
            for c in 0..3 {
                centroid[c] += d[c] * weight;
            }
            count += 1;
        }
    }
    centroid = normalize(centroid);
    let mut irradiance = [0.; 3];
    for (x, y, p) in source.enumerate_pixels() {
        let weight = omega(y, w, h) * dot(direction(x, y, w, h), centroid).max(0.);
        for c in 0..3 {
            irradiance[c] += (p[c] - sky.get_pixel(x, y)[c]) as f64 * weight;
        }
    }
    let three_irradiance = irradiance.map(|v| v * 256. / 255.);
    let intensity = three_irradiance.into_iter().fold(0_f64, f64::max);
    Ok((
        sky,
        json!({
            "peak_pixel":[px,py],"peak_rgb":peak.0,"peak_luminance":luminance(*peak),
            "seed_direction":seed,"mask_radius_degrees":1,"ring_degrees":[1,2],"ring_percentile":95,
            "local_sky_luminance_cap":cap,"ring_pixels":ring.len(),"modified_pixels":count,
            "source_direction_at_yaw_zero":centroid,"removed_normal_incidence_irradiance_rgb":irradiance,
            "three_normal_incidence_irradiance_rgb":three_irradiance,
            "sun_at_yaw_zero_and_environment_intensity_one":{"direction":centroid.map(|v|-v),"illuminance":intensity,"color":three_irradiance.map(|v|v/intensity)},
            "method":"Find brightest upper-hemisphere texel. Within 1 degree of it, cap luminance at the 95th percentile in the surrounding 1-2 degree ring, preserving chromaticity. This is an authored approximation to hidden sky. Integrate removed RGB times solid angle and cosine to the luminance-weighted source direction."
        }),
    ))
}
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 3 {
        return Err("usage: sponza_environment source.hdr separate-work-directory".into());
    }
    export(Path::new(&args[1]), Path::new(&args[2]))
}
fn export(source_path: &Path, out: &Path) -> Result<()> {
    // Inspect every destination before decoding or processing the source. This
    // includes dangling symlinks; no prior export or source alias is replaced.
    check_output_paths(out)?;
    let source = image::open(source_path)?.into_rgb32f();
    fs::create_dir_all(out)?;
    let (sky, extraction) = extract(&source)?;
    let reduced = reduce(&sky, 1024)?;
    let hdr = out.join("sky-lighting.hdr");
    // create_new also refuses an output introduced after the preflight check.
    image::codecs::hdr::HdrEncoder::new(fs::File::create_new(&hdr)?).encode(
        reduced
            .as_raw()
            .chunks_exact(3)
            .map(|p| Rgb([p[0], p[1], p[2]]))
            .collect::<Vec<_>>()
            .as_slice(),
        1024,
        512,
    )?;
    let roundtrip = image::open(&hdr)?.into_rgb32f();
    let original_display = reduce(&source, 1024)?;
    let display = image::RgbImage::from_fn(1024, 512, |x, y| {
        Rgb(original_display.get_pixel(x, y).0.map(|c| {
            let v = (c * 256. / 255.).clamp(0., 1.);
            (255.
                * if v <= 0.0031308 {
                    12.92 * v
                } else {
                    1.055 * v.powf(1. / 2.4) - 0.055
                })
            .round() as u8
        }))
    });
    display.write_to(
        &mut fs::File::create_new(out.join("sky-panorama.png"))?,
        image::ImageFormat::Png,
    )?;
    let report = json!({
        "source":{"file":"kloppenheim_05_4k.hdr","width":source.width(),"height":source.height(),"credit":"Kloppenheim 05 by Greg Zaal / Poly Haven, CC0","url":"https://polyhaven.com/a/kloppenheim_05"},
        "extraction":extraction,
        "coordinate_convention":"phi=2*pi*((x+0.5)/W-0.5), theta=pi*(y+0.5)/H; source direction=(sin(theta)*cos(phi),cos(theta),sin(theta)*sin(phi)). Source-to-world yaw: (cos(yaw)*x+sin(yaw)*z,y,-sin(yaw)*x+cos(yaw)*z). Analytic sun direction is negative world source direction. Multiply illuminance by environment intensity, with no extra division by pi.",
        "three_rgbe_factor":256./255.,
        "three_rgbe_note":"image HDR decoder uses mantissa/256; Three HDRLoader uses mantissa/255. Extraction metadata includes this scale for the analytic replacement.",
        "resize":{"width":1024,"height":512,"method":"integer box reduction in linear radiance weighted by exact source-texel solid angle","source_sky_integrated_rgb":integrated(&sky,false),"resized_sky_integrated_rgb":integrated(&reduced,false),"rgbe_roundtrip_sky_integrated_rgb":integrated(&roundtrip,false)},
        "upward_irradiance_before":integrated(&source,true),"upward_irradiance_after_extraction":integrated(&sky,true),
        "display":"sky-panorama.png is the original HDR, reduced independently before display-only linear clamp and sRGB encoding; includes the sun. Lighting alone has the sun lobe removed.",
        "lighting":"sky-lighting.hdr is an intermediate; the existing scripts/bistro-environment.ts Three common/WebGPU PMREM exporter produces the actual lighting atlas. No alternate filter or atlas resize.",
        "generation":"Explicit export only; no hashes, staleness gates, automatic rebakes or revision gates. RGBE encoding and PMREM filtering introduce additional quantization; exact final-atlas energy equality is not claimed."
    });
    fs::File::create_new(out.join("SKY-PROCESSING.json"))?
        .write_all((serde_json::to_string_pretty(&report)? + "\n").as_bytes())?;
    println!("{}", serde_json::to_string_pretty(&report["extraction"])?);
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };

    struct TestDirectory(PathBuf);
    impl TestDirectory {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "sgl-sponza-sky-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed),
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for TestDirectory {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    fn write_source(path: &Path) {
        let mut pixels = vec![Rgb([1., 2., 3.]); 1024 * 512];
        pixels[100 * 1024 + 600] = Rgb([20000., 15000., 10000.]);
        image::codecs::hdr::HdrEncoder::new(fs::File::create_new(path).unwrap())
            .encode(&pixels, 1024, 512)
            .unwrap();
    }

    #[test]
    fn existing_outputs_are_preserved_before_source_processing() {
        for name in OUTPUT_FILES {
            let root = TestDirectory::new();
            let source = root.0.join("source.hdr");
            // A malformed source would fail decoding if the preflight did not
            // inspect all three destinations first.
            fs::write(&source, b"source bytes").unwrap();
            fs::write(root.0.join(name), b"prior output").unwrap();
            let error = export(&source, &root.0).unwrap_err();
            assert!(error.to_string().contains("already exists"), "{error}");
            assert_eq!(fs::read(&source).unwrap(), b"source bytes");
            assert_eq!(fs::read(root.0.join(name)).unwrap(), b"prior output");
            assert_eq!(fs::read_dir(&root.0).unwrap().count(), 2);
        }
    }

    #[test]
    fn source_at_an_output_path_is_not_overwritten() {
        let root = TestDirectory::new();
        let source = root.0.join("sky-lighting.hdr");
        write_source(&source);
        let original = fs::read(&source).unwrap();
        assert!(export(&source, &root.0).is_err());
        assert_eq!(fs::read(source).unwrap(), original);
        assert_eq!(fs::read_dir(&root.0).unwrap().count(), 1);
    }

    #[test]
    fn hard_linked_output_does_not_modify_source() {
        let root = TestDirectory::new();
        let source = root.0.join("source.hdr");
        write_source(&source);
        let original = fs::read(&source).unwrap();
        let output = root.0.join("sky-panorama.png");
        fs::hard_link(&source, &output).unwrap();
        assert!(export(&source, &root.0).is_err());
        assert_eq!(fs::read(source).unwrap(), original);
        assert_eq!(fs::read(output).unwrap(), original);
        assert_eq!(fs::read_dir(&root.0).unwrap().count(), 2);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_outputs_including_dangling_links_are_preserved() {
        for dangling in [false, true] {
            let root = TestDirectory::new();
            let source = root.0.join("source.hdr");
            write_source(&source);
            let original = fs::read(&source).unwrap();
            let target = if dangling {
                root.0.join("missing.hdr")
            } else {
                source.clone()
            };
            let output = root.0.join("SKY-PROCESSING.json");
            std::os::unix::fs::symlink(&target, &output).unwrap();
            assert!(export(&source, &root.0).is_err());
            assert_eq!(fs::read(source).unwrap(), original);
            assert_eq!(fs::read_link(output).unwrap(), target);
            assert_eq!(fs::read_dir(&root.0).unwrap().count(), 2);
        }
    }

    #[test]
    fn invalid_source_does_not_publish_output_files() {
        let root = TestDirectory::new();
        let source = root.0.join("source.hdr");
        fs::write(&source, b"not an HDR image").unwrap();
        assert!(export(&source, &root.0).is_err());
        assert_eq!(fs::read(source).unwrap(), b"not an HDR image");
        assert_eq!(fs::read_dir(&root.0).unwrap().count(), 1);
    }

    #[test]
    fn successful_export_preserves_source_and_writes_readable_files() {
        let root = TestDirectory::new();
        let source = root.0.join("source.hdr");
        write_source(&source);
        let original = fs::read(&source).unwrap();
        let output = root.0.join("work");
        export(&source, &output).unwrap();
        assert_eq!(fs::read(source).unwrap(), original);
        for name in ["sky-lighting.hdr", "sky-panorama.png"] {
            let image = image::open(output.join(name)).unwrap();
            assert_eq!((image.width(), image.height()), (1024, 512));
        }
        let report: serde_json::Value =
            serde_json::from_slice(&fs::read(output.join("SKY-PROCESSING.json")).unwrap()).unwrap();
        assert_eq!(report["extraction"]["peak_pixel"], json!([600, 100]));
        assert_eq!(fs::read_dir(output).unwrap().count(), 3);
    }

    #[test]
    fn invalid_radiance_is_rejected() {
        for invalid in [f32::NAN, f32::INFINITY, -1.] {
            let source = Rgb32FImage::from_pixel(4, 2, Rgb([1., invalid, 1.]));
            assert_eq!(
                extract(&source).unwrap_err().to_string(),
                "invalid source radiance"
            );
        }
    }
    #[test]
    fn solid_angle_covers_sphere() {
        let sum: f64 = (0..32).map(|y| omega(y, 64, 32) * 64.).sum();
        assert!((sum - 4. * PI).abs() < 1e-12);
    }
    #[test]
    fn reduction_preserves_integrated_radiance() {
        let source = Rgb32FImage::from_fn(64, 32, |x, y| Rgb([x as f32, y as f32, (x * y) as f32]));
        let reduced = reduce(&source, 16).unwrap();
        let a = integrated(&source, false);
        let b = integrated(&reduced, false);
        for c in 0..3 {
            assert!((a[c] - b[c]).abs() / a[c] < 1e-7);
        }
    }
    #[test]
    fn sun_extraction_finds_source_peak_and_preserves_other_pixels() {
        let mut source = Rgb32FImage::from_pixel(1024, 512, Rgb([1., 2., 3.]));
        source.put_pixel(600, 100, Rgb([20000., 15000., 10000.]));
        let (sky, report) = extract(&source).unwrap();
        assert_eq!(report["peak_pixel"], json!([600, 100]));
        assert_eq!(report["modified_pixels"], 1);
        assert_eq!(sky.get_pixel(0, 0), source.get_pixel(0, 0));
        let expected = direction(600, 100, 1024, 512);
        for (c, v) in expected.into_iter().enumerate() {
            assert!(
                (report["source_direction_at_yaw_zero"][c].as_f64().unwrap() - v).abs() < 1e-12
            );
        }
        let e = report["removed_normal_incidence_irradiance_rgb"][0]
            .as_f64()
            .unwrap();
        let expected_e = (source.get_pixel(600, 100)[0] - sky.get_pixel(600, 100)[0]) as f64
            * omega(100, 1024, 512);
        assert!((e - expected_e).abs() < 1e-10);
    }
}
