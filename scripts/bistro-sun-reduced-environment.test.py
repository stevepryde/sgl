"""Focused numerical tests for the explicit HDR content transformation."""
import importlib.util
import json
import math
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

import cv2
import numpy as np

spec = importlib.util.spec_from_file_location("bistro_sun", Path(__file__).with_name("bistro-sun-reduced-environment.py"))
sun = importlib.util.module_from_spec(spec)
spec.loader.exec_module(sun)


class SunReductionTests(unittest.TestCase):
    def test_finalize_preserves_actual_exporter_backend_provenance(self):
        with tempfile.TemporaryDirectory(prefix="bistro-sun-finalize-") as temporary:
            root = Path(temporary)
            for name in ["raw-original", "aligned-only", "sun-reduced/pmrem-bake"]:
                (root / name).mkdir(parents=True)
            atlas = np.zeros((2, 4), dtype="<f2").tobytes()
            (root / "raw-original/pmrem.rgba16").write_bytes(atlas)
            baked = root / "sun-reduced/pmrem-bake"
            (baked / "pmrem.rgba16").write_bytes(atlas)
            (baked / "THREE-LICENSE.txt").write_text("actual bake licence")
            backend = {"backend": "WebGPU", "backend_asserted": True, "adapter_available": True, "probed_adapter": {"vendor": "test-vendor"}}
            (baked / "environment.json").write_text(json.dumps({"width": 2, "height": 1, "backend": backend, "generator": "actual exporter generator"}))
            (root / "ENVIRONMENT-PROCESSING.json").write_text('{"three_pipeline":{}}')
            sun.finalize(root)
            result = json.loads((root / "sun-reduced/environment.json").read_text())
            self.assertEqual(result["backend"], backend)
            self.assertEqual(result["generator"], "actual exporter generator")
            self.assertEqual(result["asset_variant"], "sun-reduced")
            self.assertEqual((root / "sun-reduced/pmrem.rgba16").read_bytes(), atlas)

    def test_explicit_rerun_invalidates_published_lighting_and_old_bake_only(self):
        with tempfile.TemporaryDirectory(prefix="bistro-sun-rerun-") as temporary:
            root = Path(temporary)
            original = root / "original"
            original.mkdir()
            rgb = np.ones((360, 720, 3), dtype=np.float32)
            x = int((math.atan2(sun.SUN_SEED[2], sun.SUN_SEED[0]) / (2 * math.pi) + .5) * 720)
            y = int(math.acos(sun.SUN_SEED[1]) / math.pi * 360)
            rgb[y, x] = [200, 100, 50]
            source = original / "source.hdr"
            self.assertTrue(cv2.imwrite(str(source), rgb[:, :, ::-1]))
            (original / "environment.json").write_text('{"width":4,"height":2}')
            for name in ["panorama.png", "THREE-LICENSE.txt", "LICENSE.txt", "SGL-ATTRIBUTION.txt", "pmrem.rgba16"]:
                (original / name).write_bytes(("original " + name).encode())
            originals = {path.name: path.read_bytes() for path in original.iterdir()}
            output = root / "experiment"
            for directory in [output / "sun-reduced", output / "sun-reduced/pmrem-bake"]:
                directory.mkdir(parents=True, exist_ok=True)
                for name in ["environment.json", "pmrem.rgba16"]:
                    (directory / name).write_bytes(b"stale derived lighting")
                (directory / "keep.txt").write_text("not an invalidated artifact")
            result = subprocess.run([sys.executable, str(Path(sun.__file__)), "--source", str(source), "--original-environment", str(original), "--output", str(output)], capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            for directory in [output / "sun-reduced", output / "sun-reduced/pmrem-bake"]:
                self.assertFalse((directory / "environment.json").exists())
                self.assertFalse((directory / "pmrem.rgba16").exists())
                self.assertTrue((directory / "keep.txt").exists())
            for control in ["raw-original", "aligned-only"]:
                self.assertEqual((output / control / "pmrem.rgba16").read_bytes(), originals["pmrem.rgba16"])
                self.assertTrue((output / control / "environment.json").exists())
            result = subprocess.run([sys.executable, str(Path(sun.__file__)), "--output", str(output), "--finalize-pmrem"], capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertFalse((output / "sun-reduced/environment.json").exists())
            self.assertEqual({path.name: path.read_bytes() for path in original.iterdir()}, originals)

    def test_output_aliases_are_rejected_before_invalidating_sources(self):
        with tempfile.TemporaryDirectory(prefix="bistro-sun-alias-") as temporary:
            root = Path(temporary)
            source = root / "source.hdr"
            source.write_bytes(b"source")
            original = root / "original"
            original.mkdir()
            alias = root / "alias"
            alias.symlink_to(original, target_is_directory=True)
            for output in [root, original, alias, original / "nested"]:
                with self.assertRaisesRegex(ValueError, "separate"):
                    sun.validated_output(output, source, original)
            output = root / "safe"
            output.mkdir()
            (output / "sun-reduced").symlink_to(original, target_is_directory=True)
            with self.assertRaisesRegex(ValueError, "symlink"):
                sun.validated_output(output, source, original)
            self.assertEqual(source.read_bytes(), b"source")

    def test_latlong_solid_angles_cover_sphere_and_constant_hemisphere(self):
        # Independent physical integrals: a sphere has 4*pi solid angle;
        # unit radiance produces pi upward irradiance. Wrong cos/row weights
        # or a missing longitude factor break these even for constant skies.
        _, _, omega = sun.geometry(360, 720)
        self.assertAlmostEqual(float(omega.sum() * 720), 4 * math.pi, places=12)
        irradiance = sun.upward_irradiance(np.ones((360, 720, 3), dtype=np.float32))
        np.testing.assert_allclose(irradiance, [math.pi] * 3, rtol=1e-5)

    def test_cap_changes_only_bright_sun_and_preserves_chromaticity(self):
        rgb = np.empty((360, 720, 3), dtype=np.float32)
        rgb[:] = [1, 2, 3]
        x = int((math.atan2(sun.SUN_SEED[2], sun.SUN_SEED[0]) / (2 * math.pi) + .5) * 720)
        y = int(math.acos(sun.SUN_SEED[1]) / math.pi * 360)
        rgb[y, x] = [200, 100, 50]
        result, inside, metadata = sun.cap_sun(rgb)
        changed = np.any(result != rgb, axis=2)
        self.assertEqual(int(changed.sum()), 1)
        self.assertTrue(inside[y, x])
        np.testing.assert_array_equal(result[~inside], rgb[~inside])
        self.assertAlmostEqual(float(result[y, x, 0] / result[y, x, 1]), 2)
        self.assertAlmostEqual(float(result[y, x, 1] / result[y, x, 2]), 2)
        self.assertTrue(np.all(result[y, x] > 0))

    def test_single_pixel_extraction_keeps_rgb_energy_ratio_and_known_direction(self):
        before = np.zeros((360, 720, 3), dtype=np.float32)
        before[180, 360] = [10, 5, 2]
        direction, irradiance, changed = sun.extracted_sun(before, np.zeros_like(before))
        a = math.radians(.25)
        np.testing.assert_allclose(direction, [math.cos(a) ** 2, -math.sin(a), math.sin(a) * math.cos(a)], atol=1e-12)
        np.testing.assert_allclose(irradiance / irradiance.max(), [1, .5, .2], atol=1e-12)
        self.assertEqual(int(changed.sum()), 1)

    def test_empty_extraction_or_invalid_intensity_reports_clear_errors(self):
        blank = np.zeros((360, 720, 3), dtype=np.float32)
        with self.assertRaisesRegex(ValueError, "sky ring"):
            sun.cap_sun(blank)
        with self.assertRaisesRegex(ValueError, "removed no radiance"):
            sun.extracted_sun(blank, blank)
        with self.assertRaisesRegex(ValueError, "sun mask"):
            sun.cap_sun(np.ones((2, 4, 3), dtype=np.float32))
        for intensity in [0, -1, float("nan"), float("inf")]:
            with self.assertRaisesRegex(ValueError, "finite and positive"):
                sun.sun_at_yaw(sun.SUN_SEED, np.ones(3), intensity, 0)


if __name__ == "__main__":
    unittest.main()
