# Builds the DracoSphere fixture in Blender and exports it with Draco mesh
# compression (the exporter's defaults) and without it, the uncompressed
# reference, then the sphere alone, uncompressed, as gltf-pipeline's input
# (README.md). Run from this directory:
#   Blender --background --factory-startup --python export.py
import os
import bpy

out = os.path.dirname(os.path.abspath(__file__))
bpy.ops.wm.read_factory_settings(use_empty=True)

# A UV sphere: smooth normals, and the default UV layout's seam along one
# meridian and at the poles, where one position takes several UVs.
bpy.ops.mesh.primitive_uv_sphere_add(segments=32, ring_count=16, radius=1.0, location=(0, 0, 0))
sphere = bpy.context.object
sphere.name = "Sphere"
bpy.ops.object.shade_smooth()

# A cube with one shape key: flat normals, UV seams on every edge, and a
# morph target, for which the exporter keeps the triangle order and Draco
# encodes sequentially.
bpy.ops.mesh.primitive_cube_add(size=1.0, location=(3, 0, 0))
cube = bpy.context.object
cube.name = "Cube"
cube.shape_key_add(name="Basis")
key = cube.shape_key_add(name="Stretch")
for point in key.data:
    point.co.z *= 2.0
key.value = 0.25


def export(name, draco, selection=False):
    bpy.ops.export_scene.gltf(
        filepath=os.path.join(out, name),
        export_format="GLTF_SEPARATE",
        export_draco_mesh_compression_enable=draco,
        export_materials="NONE",
        export_animations=False,
        use_selection=selection,
    )


export("glTF/DracoSphere.gltf", False)
export("glTF-Draco/DracoSphere.gltf", True)
bpy.ops.object.select_all(action="DESELECT")
sphere.select_set(True)
export("glTF-Sphere/DracoSphere.gltf", False, selection=True)
