"""Offline exact-block companions and lossy metrics. No runtime dependency.

Tools: Pillow, NumPy and texture2ddecoder==1.0.6 in an ignored export venv.
texture2ddecoder returns BGRA; convert channels explicitly before PNG output.
"""
import argparse
import importlib.metadata
import json
import math
from pathlib import Path
import struct
import subprocess
import tempfile

import numpy as np
from PIL import Image
import texture2ddecoder


def decode_blocks(blocks, width, height, kind):
    if kind == "bc7":
        bgra = texture2ddecoder.decode_bc7(blocks, width, height)
    elif kind == "etc2":
        bgra = texture2ddecoder.decode_etc2a8(blocks, width, height)
    elif kind == "astc4x4":
        bgra = texture2ddecoder.decode_astc(blocks, width, height, 4, 4)
    else:
        raise ValueError(f"unsupported block format: {kind}")
    return Image.frombytes("RGBA", (width, height), bgra, "raw", "BGRA")


def error_metrics(reference, decoded, normal):
    a = np.asarray(reference, dtype=np.float32)
    b = np.asarray(decoded, dtype=np.float32)
    error = b - a
    mse = float(np.mean(error[:, :, :3] ** 2))
    result = {
        "rgb_rmse_codes": math.sqrt(mse),
        "rgb_psnr_db": None if mse == 0 else 10 * math.log10(255 * 255 / mse),
        "alpha_mean_abs_codes": float(np.mean(np.abs(error[:, :, 3]))),
        "alpha_max_abs_codes": float(np.max(np.abs(error[:, :, 3]))),
        "alpha_half_coverage_changed_pixels": int(np.count_nonzero((a[:, :, 3] >= 127.5) != (b[:, :, 3] >= 127.5))),
        "reference_alpha_range": [int(a[:, :, 3].min()), int(a[:, :, 3].max())],
        "decoded_alpha_range": [int(b[:, :, 3].min()), int(b[:, :, 3].max())],
    }
    if normal:
        na, nb = a[:, :, :3] / 127.5 - 1, b[:, :, :3] / 127.5 - 1
        na /= np.maximum(np.linalg.norm(na, axis=2, keepdims=True), 1e-12)
        nb /= np.maximum(np.linalg.norm(nb, axis=2, keepdims=True), 1e-12)
        degrees = np.degrees(np.arccos(np.clip(np.sum(na * nb, axis=2), -1, 1)))
        result["normal_angular_error_degrees"] = {"mean": float(degrees.mean()), "p95": float(np.percentile(degrees, 95)), "max": float(degrees.max())}
    return result


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("package", type=Path)
    parser.add_argument("prepared", type=Path)
    parser.add_argument("--ktx", required=True)
    args = parser.parse_args()
    manifest = json.loads((args.package / "compressed-images.json").read_text())
    observations = []
    companion_bytes = companion_file_bytes = 0
    with tempfile.TemporaryDirectory(prefix="sgl-block-decode-") as scratch:
        for entry in manifest["images"]:
            source = args.package / entry["ktx2"]
            if manifest["format"] == "astc4x4":
                # libktx's ASTC decoder rounds normalized components; the
                # independent Python decoder can differ by one 8-bit code.
                # Use the official KTX decoder for the stored companion.
                raster = Path(scratch) / "decoded.png"
                subprocess.run([args.ktx, "extract", "--level", "0", str(source), str(raster)], check=True, capture_output=True)
                decoded = Image.open(raster).convert("RGBA")
            else:
                raw = Path(scratch) / "blocks.bin"
                subprocess.run([args.ktx, "extract", "--raw", "--level", "0", str(source), str(raw)], check=True, capture_output=True)
                decoded = decode_blocks(raw.read_bytes(), entry["width"], entry["height"], manifest["format"])
            reference = Image.open(args.prepared / entry["source_png"]).convert("RGBA")
            if "decoded_level_0" in entry:
                destination = args.package / entry["decoded_level_0"]
                decoded.save(destination)
                companion_bytes += entry["width"] * entry["height"] * 4
                companion_file_bytes += destination.stat().st_size
            observations.append({"index": entry["index"], "source_png": entry["source_png"], "roles": entry["roles"], **error_metrics(reference, decoded, "normal" in entry["roles"])})
    summary = {}
    for role in ["color", "normal", "data"]:
        items = [item for item in observations if role in item["roles"]]
        if not items:
            continue
        summary[role] = {
            "images": len(items),
            "mean_image_rgb_rmse_codes": sum(item["rgb_rmse_codes"] for item in items) / len(items),
            "worst_image_rgb_rmse_codes": max(item["rgb_rmse_codes"] for item in items),
            "alpha_half_coverage_changed_pixels": sum(item["alpha_half_coverage_changed_pixels"] for item in items),
            "worst_alpha_error_codes": max(item["alpha_max_abs_codes"] for item in items),
        }
        if role == "normal":
            summary[role]["mean_image_normal_angle_degrees"] = sum(item["normal_angular_error_degrees"]["mean"] for item in items) / len(items)
            summary[role]["worst_image_normal_p95_degrees"] = max(item["normal_angular_error_degrees"]["p95"] for item in items)
    report = {"format": manifest["format"], "lossy": True, "level": 0, "notes": ["Metrics measure encoded-channel level-zero error, not visual acceptance or pixel equivalence.", "All mip inputs use SGL's actual Rgba8MipChain colour/data filter; normals retain XYZ, never GA repacked.", "ETC2/ASTC companions decode final stored blocks, not the original PNG."], "summary": summary, "images": observations}
    (args.package / "compression-quality.json").write_text(json.dumps(report, indent=2, allow_nan=False) + "\n")
    stats_path = args.package / "asset-stats.json"
    stats = json.loads(stats_path.read_text())
    stats["compression"]["retainedCompanionBytes"] = companion_bytes
    stats["compression"]["companionFileBytes"] = companion_file_bytes
    stats_path.write_text(json.dumps(stats, indent=2) + "\n")
    metadata = importlib.metadata.metadata("texture2ddecoder")
    provenance = {"decoder": "official Khronos KTX ASTC decoder" if manifest["format"] == "astc4x4" else "texture2ddecoder", "texture2ddecoder_version": texture2ddecoder.__version__, "source": "https://github.com/K0lb3/texture2ddecoder", "package": f"https://pypi.org/project/texture2ddecoder/{texture2ddecoder.__version__}/", "license": "MIT", "channel_order": "texture2ddecoder BGRA explicitly converted to PNG RGBA; KTX ASTC PNG preserves encoded channels", "numpy_version": np.__version__, "pillow_version": importlib.metadata.version("Pillow"), "use": "Offline final-block decode and metrics only; no browser/game runtime dependency."}
    provenance["ktx"] = {"version": manifest["encoder"]["ktx"], "source": "https://github.com/KhronosGroup/KTX-Software/tree/v4.4.2", "license": "KTX-specific files generally Apache-2.0; bundled components have their own licences. External tool code is not included in this asset package.", "license_source": "Installed KTX-Software 4.4.2 share/doc/KTX-Software/html/license.html"}
    (args.package / "COMPRESSION-TOOLS.json").write_text(json.dumps(provenance, indent=2) + "\n")
    (args.package / "TEXTURE2DDECODER-LICENSE.txt").write_text(metadata["License"] + "\n\nBCn, ETC and ASTC decoders are MIT-derived; see upstream package README for source credits.\n")
    print(json.dumps({"format": manifest["format"], "summary": summary, "companion_bytes": companion_bytes}))


if __name__ == "__main__":
    main()
