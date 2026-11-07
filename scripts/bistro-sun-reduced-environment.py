"""Explicit Bistro-only HDR sun extraction experiment. No GPU or rebake here.

The optional finalize step consumes an actual existing Three WebGPU PMREM bake;
it never resizes an atlas or substitutes another generator.
"""
import argparse
import hashlib
import json
import math
from pathlib import Path
import shutil

import cv2
import numpy as np

SUN_SEED = np.array([0.7112713028519169, 0.4790422039711839, 0.5144042190279617])
THREE_RGBE_FACTOR = 256 / 255
AUTHORED_YAW = 2.84045999560719


def validated_output(root, source, original_environment):
    root = root.resolve()
    source = source.resolve(strict=True)
    original_environment = original_environment.resolve(strict=True)
    if source.is_relative_to(root) or original_environment.is_relative_to(root) or root.is_relative_to(original_environment):
        raise ValueError("output must be separate from source HDR and original environment")
    # Refuse redirected output directories before invalidating known generated
    # files. In particular, never remove a source reached through a symlink.
    for name in ["raw-original", "aligned-only", "sun-reduced", "sun-reduced/pmrem-bake"]:
        directory = root / name
        if directory.resolve() != directory:
            raise ValueError("output variant and bake directories must not be symlink aliases")
    inputs = [source] + [path for path in original_environment.iterdir() if path.is_file()]
    files = ["panorama.png", "THREE-LICENSE.txt", "LICENSE.txt", "SGL-ATTRIBUTION.txt", "HDRI-ATTRIBUTION.txt", "environment.json", "pmrem.rgba16", "san_giuseppe_bridge_4k.hdr", "san_giuseppe_bridge_4k-sun-reduced.hdr", "ENVIRONMENT-PROCESSING.json"]
    outputs = [root / "ENVIRONMENT-PROCESSING.json"] + [root / variant / name for variant in ["raw-original", "aligned-only", "sun-reduced", "sun-reduced/pmrem-bake"] for name in files]
    for path in outputs:
        if path.is_symlink() or (path.exists() and any(path.samefile(source_file) for source_file in inputs)):
            raise ValueError("output file must not alias a source file or use a symlink")
    return root


def invalidate_derived_lighting(root):
    # An explicit preprocessing invocation invalidates only this experiment's
    # published derived lighting and prior bake. A new bake plus finalize is
    # then required; an old pmrem-bake cannot silently republish old lighting.
    for directory in [root / "sun-reduced", root / "sun-reduced" / "pmrem-bake"]:
        for name in ["environment.json", "pmrem.rgba16"]:
            (directory / name).unlink(missing_ok=True)


def sha256(path):
    with Path(path).open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest() if hasattr(hashlib, "file_digest") else hashlib.sha256(stream.read()).hexdigest()


def read_hdr(path):
    bgr = cv2.imread(str(path), cv2.IMREAD_UNCHANGED)
    if bgr is None or bgr.dtype != np.float32 or bgr.ndim != 3 or bgr.shape[2] != 3:
        raise ValueError("expected a linear RGB Radiance HDR image")
    if not np.isfinite(bgr).all() or np.any(bgr < 0):
        raise ValueError("HDR contains negative or non-finite radiance")
    return bgr[:, :, ::-1]


def geometry(height, width):
    theta = (np.arange(height) + 0.5) * np.pi / height
    phi = ((np.arange(width) + 0.5) / width - 0.5) * 2 * np.pi
    omega = (2 * np.pi / width) * (np.cos(np.arange(height) * np.pi / height) - np.cos((np.arange(height) + 1) * np.pi / height))
    return theta, phi, omega


def upward_irradiance(rgb):
    height, width = rgb.shape[:2]
    theta, _, omega = geometry(height, width)
    return np.sum(rgb.sum(axis=1, dtype=np.float64) * (omega * np.maximum(np.cos(theta), 0))[:, None], axis=0)


def cap_sun(rgb):
    height, width = rgb.shape[:2]
    theta, phi, omega = geometry(height, width)
    dot = np.sin(theta)[:, None] * (np.cos(phi)[None, :] * SUN_SEED[0] + np.sin(phi)[None, :] * SUN_SEED[2]) + np.cos(theta)[:, None] * SUN_SEED[1]
    inside = dot > np.cos(np.deg2rad(1))
    ring = (dot <= np.cos(np.deg2rad(1))) & (dot > np.cos(np.deg2rad(2)))
    if not inside.any() or not ring.any():
        raise ValueError("HDR resolution does not contain pixels in the fixed Bistro sun mask and surrounding ring")
    luminance = .2126 * rgb[:, :, 0].astype(np.float64) + .7152 * rgb[:, :, 1] + .0722 * rgb[:, :, 2]
    cap = float(np.percentile(luminance[ring], 95, method="linear"))
    if not math.isfinite(cap) or cap <= 0:
        raise ValueError("surrounding sky ring must provide a positive finite luminance cap")
    y, x = np.where(inside & (luminance > cap))
    if len(y) == 0:
        raise ValueError("fixed Bistro sun mask contains no radiance above the surrounding sky cap")
    result = rgb.copy()
    result[y, x] *= (cap / luminance[y, x])[:, None]
    return result, inside, {"mask_degrees": 1, "ring_degrees": [1, 2], "ring_percentile": 95, "percentile_method": "linear", "luminance_coefficients": [.2126, .7152, .0722], "luminance_cap_radiance_scale": cap, "ring_pixels": int(ring.sum()), "mask_pixels": int(inside.sum()), "intended_modified_pixels": len(y), "seed_direction_at_yaw_zero": SUN_SEED.tolist()}


def extracted_sun(original, sky):
    height, width = original.shape[:2]
    theta, phi, omega = geometry(height, width)
    changed = np.any(original != sky, axis=2)
    y, x = np.where(changed)
    if len(y) == 0:
        raise ValueError("HDR transformation removed no radiance; no sun can be calibrated")
    excess = original[y, x].astype(np.float64) - sky[y, x]
    if np.any(excess < 0):
        raise ValueError("HDR roundtrip increased a channel; extraction must only remove radiance")
    direction = np.stack([np.sin(theta[y]) * np.cos(phi[x]), np.cos(theta[y]), np.sin(theta[y]) * np.sin(phi[x])], axis=1)
    luminance = .2126 * excess[:, 0] + .7152 * excess[:, 1] + .0722 * excess[:, 2]
    centroid = np.sum((luminance * omega[y])[:, None] * direction, axis=0)
    length = np.linalg.norm(centroid)
    if not np.isfinite(length) or length <= 0:
        raise ValueError("removed radiance has no positive finite sun centroid")
    centroid /= length
    irradiance = np.sum(excess * (np.maximum(np.sum(direction * centroid, axis=1), 0) * omega[y])[:, None], axis=0)
    if not np.isfinite(irradiance).all() or irradiance.max() <= 0:
        raise ValueError("removed sun has no positive finite normal-incidence irradiance")
    return centroid, irradiance, changed


def sun_at_yaw(direction, irradiance, environment_strength, yaw):
    if not math.isfinite(environment_strength) or environment_strength <= 0:
        raise ValueError("environment intensity must be finite and positive")
    c, s = math.cos(yaw), math.sin(yaw)
    world = np.array([c * direction[0] + s * direction[2], direction[1], -s * direction[0] + c * direction[2]])
    scaled = irradiance * THREE_RGBE_FACTOR * environment_strength
    return {"environment_yaw": yaw, "environment_intensity": environment_strength, "source_direction_world": world.tolist(), "sun": {"direction": (-world).tolist(), "illuminance": float(scaled.max()), "color": (scaled / scaled.max()).tolist()}, "energy_convention": "Measured RGBE-roundtrip removed irradiance times Three HDRLoader 256/255 and environment intensity; no extra division by pi"}


def atlas_stats(path):
    pixels = np.fromfile(path, dtype="<f2").reshape(-1, 4)[:, :3]
    return {"bytes": Path(path).stat().st_size, "finite": bool(np.isfinite(pixels).all()), "maximum_rgb": float(pixels.max()), "minimum_rgb": float(pixels.min()), "rgb_components_at_fp16_maximum": int(np.count_nonzero(pixels == 65504)), "sha256": sha256(path), "note": "Atlas storage/filtering metrics, separate from source-domain energy conservation"}


def finalize(root):
    output = root / "sun-reduced"
    baked = output / "pmrem-bake"
    metadata = json.loads((baked / "environment.json").read_text())
    shutil.copyfile(baked / "pmrem.rgba16", output / "pmrem.rgba16")
    shutil.copyfile(baked / "THREE-LICENSE.txt", output / "THREE-LICENSE.txt")
    metadata.update({"source": "san_giuseppe_bridge_4k-sun-reduced.hdr; explicit Bistro sun-cap experiment", "display_panorama": "Unmodified original San Giuseppe Bridge display panorama", "processing_metadata": "ENVIRONMENT-PROCESSING.json", "asset_variant": "sun-reduced"})
    (output / "environment.json").write_text(json.dumps(metadata, indent=2) + "\n")
    report_path = root / "ENVIRONMENT-PROCESSING.json"
    report = json.loads(report_path.read_text())
    report["three_pipeline"]["status"] = "Sun-reduced PMREM baked explicitly with the existing Three common/WebGPU exporter and finalized"
    report["pmrem"] = {name: atlas_stats(root / name / "pmrem.rgba16") for name in ["raw-original", "sun-reduced"]}
    report["pmrem"]["note"] = "Only sun-reduced was rebaked. Raw-original/aligned-only retain the original atlas. Final-atlas energy equality is not claimed: filtering and FP16 storage can differ."
    report_path.write_text(json.dumps(report, indent=2) + "\n")
    for name in ["raw-original", "aligned-only", "sun-reduced"]:
        shutil.copyfile(report_path, root / name / "ENVIRONMENT-PROCESSING.json")
    print(json.dumps({"finalized": str(output), "pmrem": report["pmrem"]}))


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--source", type=Path)
    parser.add_argument("--original-environment", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--environment-intensity", type=float, default=.7)
    parser.add_argument("--finalize-pmrem", action="store_true")
    args = parser.parse_args()
    if not math.isfinite(args.environment_intensity) or args.environment_intensity <= 0:
        parser.error("--environment-intensity must be finite and positive")
    if args.finalize_pmrem:
        finalize(args.output)
        return
    if args.source is None or args.original_environment is None:
        parser.error("--source and --original-environment are required for preprocessing")
    args.output = validated_output(args.output, args.source, args.original_environment)
    original = read_hdr(args.source)
    reduced, inside, method = cap_sun(original)
    invalidate_derived_lighting(args.output)
    for name in ["raw-original", "aligned-only", "sun-reduced"]:
        directory = args.output / name
        directory.mkdir(parents=True, exist_ok=True)
        for file in ["panorama.png", "THREE-LICENSE.txt", "LICENSE.txt", "SGL-ATTRIBUTION.txt"]:
            shutil.copyfile(args.original_environment / file, directory / file)
        (directory / "HDRI-ATTRIBUTION.txt").write_text("San Giuseppe Bridge, Greg Zaal, HDRIHaven / Poly Haven, CC0.\nhttps://polyhaven.com/a/san_giuseppe_bridge\nhttps://polyhaven.com/license\nPublished with the Bistro distribution. Bistro licensing/provenance remains separate in LICENSE.txt and SGL-ATTRIBUTION.txt.\nThis explicit experiment caps a small sun lobe only for lighting; its original display panorama is retained. See ENVIRONMENT-PROCESSING.json.\n")
        if name != "sun-reduced":
            shutil.copyfile(args.source, directory / "san_giuseppe_bridge_4k.hdr")
            shutil.copyfile(args.original_environment / "pmrem.rgba16", directory / "pmrem.rgba16")
            metadata = json.loads((args.original_environment / "environment.json").read_text())
            metadata.update({"asset_variant": name, "processing_metadata": "ENVIRONMENT-PROCESSING.json", "note": "Unmodified original environment; aligned-only changes the game-authored analytic sun, not environment pixels"})
            (directory / "environment.json").write_text(json.dumps(metadata, indent=2) + "\n")
    derived = args.output / "sun-reduced" / "san_giuseppe_bridge_4k-sun-reduced.hdr"
    if not cv2.imwrite(str(derived), reduced[:, :, ::-1], [cv2.IMWRITE_HDR_COMPRESSION, cv2.IMWRITE_HDR_COMPRESSION_RLE]):
        raise RuntimeError("HDR writer failed")
    roundtrip = read_hdr(derived)
    centroid, irradiance, changed = extracted_sun(original, roundtrip)
    outside_changed = int(np.count_nonzero(changed & ~inside))
    if outside_changed:
        raise ValueError(f"RGBE roundtrip changed {outside_changed} pixels outside the declared sun mask")
    before = upward_irradiance(original)
    after = upward_irradiance(roundtrip)
    approximation = irradiance * centroid[1]
    report = {"source": {"file": args.source.name, "sha256": sha256(args.source), "width": original.shape[1], "height": original.shape[0], "credit": "San Giuseppe Bridge by Greg Zaal / Poly Haven, CC0", "url": "https://polyhaven.com/a/san_giuseppe_bridge"}, "derived": {"file": derived.name, "sha256": sha256(derived), "rgb_maximum": float(roundtrip.max()), "modified_pixels_after_rgbe_roundtrip": int(changed.sum()), "outside_mask_changed_pixels": outside_changed}, "method": method, "method_note": "Inside the 1 degree cone, scale RGB by min(1, local luminance cap / luminance), preserving chromaticity. The local sky cap is an explicit authored approximation, not recovered hidden sky or a renderer algorithm.", "coordinate_convention": "Pixel-centre phi=2*pi*((x+.5)/W-.5), theta=pi*(y+.5)/H; direction=(sin(theta)*cos(phi),cos(theta),sin(theta)*sin(phi)); yaw uses the inverse of SGL environment lookup rotation", "solid_angle": "(2*pi/W)*(cos(pi*y/H)-cos(pi*(y+1)/H))", "radiance_scale_metrics_after_rgbe_roundtrip": {"sun_centroid": centroid.tolist(), "removed_normal_incidence_irradiance_rgb": irradiance.tolist(), "upward_irradiance_div_pi_before": (before / np.pi).tolist(), "upward_irradiance_div_pi_after": (after / np.pi).tolist(), "analytic_replacement_upward_relative_error": ((approximation - (before - after)) / (before - after)).tolist()}, "three_pipeline": {"version": "0.185.1", "rgbe_factor_relative_to_opencv": THREE_RGBE_FACTOR, "reason": "Actual Three HDRLoader.js decodes RGBE mantissas /255; OpenCV source metrics use /256", "pmrem_generator": "Existing Three common/WebGPU PMREMGenerator; no WebGL substitution or atlas resizing", "display_panorama": "Unmodified original panorama, independently preserved", "status": "PMREM bake pending; sun-reduced environment.json is published only after explicit finalize"}, "calibrated_sun": {"yaw_zero": sun_at_yaw(centroid, irradiance, args.environment_intensity, 0), "authored_yaw": sun_at_yaw(centroid, irradiance, args.environment_intensity, AUTHORED_YAW)}, "controls": {"raw_original": "Original environment and caller's original analytic sun", "aligned_only": "Identical original HDR/panorama/PMREM; align analytic light direction to extracted source direction, preserving the control's intensity/color (4.5 and [1,.94,.85] for the Bevy-camera control)", "sun_reduced": "Derived lighting HDR/PMREM plus the calibrated aligned directional light; original display panorama"}, "references": ["https://unity.com/blog/engine-platform/practical-approach-to-physically-based-shading", "https://docs.blender.org/manual/id/4.5/render/eevee/world_settings.html#sun"], "tools": {"opencv": cv2.__version__, "numpy": np.__version__}, "provenance_only": "Source hashes record this explicit export; no automatic staleness rejection or rebake"}
    path = args.output / "ENVIRONMENT-PROCESSING.json"
    path.write_text(json.dumps(report, indent=2) + "\n")
    for name in ["raw-original", "aligned-only", "sun-reduced"]:
        shutil.copyfile(path, args.output / name / path.name)
    print(json.dumps({"derived": report["derived"], "method": method, "metrics": report["radiance_scale_metrics_after_rgbe_roundtrip"], "calibrated_sun": report["calibrated_sun"]}))


if __name__ == "__main__":
    main()
