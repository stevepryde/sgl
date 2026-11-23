# Writes glTF-Expanded/ from glTF/: Khronos glTF Blender I/O imports the
# instanced node as one object per instance and exports them as ordinary
# nodes (README.md). Run from this directory:
#   Blender --background --factory-startup --python expand.py
import os
import bpy

out = os.path.dirname(os.path.abspath(__file__))
bpy.ops.wm.read_factory_settings(use_empty=True)
bpy.ops.import_scene.gltf(filepath=os.path.join(out, "glTF", "SimpleInstancing.gltf"))
os.makedirs(os.path.join(out, "glTF-Expanded"), exist_ok=True)
bpy.ops.export_scene.gltf(
    filepath=os.path.join(out, "glTF-Expanded", "SimpleInstancing.gltf"),
    export_format="GLTF_SEPARATE",
    export_gpu_instances=False,
    export_materials="NONE",
    export_texcoords=False,
)
