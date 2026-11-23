# Builds the InstancedProps fixture in Blender and exports it twice with
# Khronos glTF Blender I/O: with GPU instances (EXT_mesh_gpu_instancing) and
# without, as ordinary nodes (README.md). Run from this directory:
#   Blender --background --factory-startup --python export.py
import math
import os
import bpy

out = os.path.dirname(os.path.abspath(__file__))
bpy.ops.wm.read_factory_settings(use_empty=True)


def material(name, color):
    result = bpy.data.materials.new(name)
    result.use_nodes = True
    bsdf = next(n for n in result.node_tree.nodes if n.type == "BSDF_PRINCIPLED")
    bsdf.inputs[0].default_value = color
    # Single-sided, so a face drawn with the wrong winding is culled.
    result.use_backface_culling = True
    return result


# The prop: an L of two boxes, asymmetric on every axis, so a mirrored
# instance differs from an unmirrored one.
bpy.ops.mesh.primitive_cube_add(size=1.0, location=(0.35, 0.0, 0.0), scale=(1.0, 0.3, 0.3))
bar = bpy.context.object
bpy.ops.mesh.primitive_cube_add(size=1.0, location=(-0.0, 0.0, 0.25), scale=(0.3, 0.3, 0.8))
post = bpy.context.object
bpy.ops.object.select_all(action="DESELECT")
bar.select_set(True)
post.select_set(True)
bpy.context.view_layer.objects.active = bar
bpy.ops.object.join()
prop = bpy.context.object
prop.name = "Prop"
bpy.ops.object.transform_apply(location=True, rotation=True, scale=True)
prop.data.name = "Prop"
prop.data.materials.append(material("Prop", (0.8, 0.5, 0.2, 1.0)))


def grove(name, location, rotation, scale, instances):
    """An empty placing linked duplicates of the prop, each at its own
    translation, rotation (XYZ Euler, radians) and scale."""
    empty = bpy.data.objects.new(name, None)
    bpy.context.scene.collection.objects.link(empty)
    empty.location = location
    empty.rotation_euler = rotation
    empty.scale = scale
    for index, (t, r, s) in enumerate(instances):
        child = bpy.data.objects.new(f"{name}.{index}", prop.data)
        bpy.context.scene.collection.objects.link(child)
        child.parent = empty
        child.location = t
        child.rotation_euler = r
        child.scale = s
    return empty


# A turned, non-uniformly scaled parent: translated, rotated, scaled and
# mirrored instances (a negative scale on one axis, and on all three).
grove(
    "Grove",
    (1.0, -0.5, 0.2),
    (0.0, 0.0, 0.4),
    (1.2, 0.9, 1.1),
    [
        ((0.0, 0.0, 0.0), (0.0, 0.0, 0.0), (1.0, 1.0, 1.0)),
        ((2.0, 0.5, 0.0), (0.0, 0.0, 1.2), (1.0, 1.0, 1.0)),
        ((-1.5, 1.0, 0.3), (0.3, -0.2, 2.5), (0.8, 1.3, 0.9)),
        ((0.5, -2.0, 0.0), (0.0, 0.0, 0.7), (-1.0, 1.0, 1.0)),
        ((-2.0, -1.5, 0.1), (0.1, 0.0, -0.9), (1.0, -1.0, -1.0)),
        ((2.5, -1.0, 0.0), (0.0, 0.0, 2.0), (1.1, 0.7, -1.4)),
    ],
)
# A second instanced node of the same mesh, at the scene's root frame.
grove(
    "Copse",
    (-3.0, 3.0, 0.0),
    (0.0, 0.0, 0.0),
    (1.0, 1.0, 1.0),
    [
        ((0.0, 0.0, 0.0), (0.0, 0.0, 0.5), (1.0, 1.0, 1.0)),
        ((1.5, 0.5, 0.0), (0.0, 0.0, -0.5), (-1.0, 1.0, 1.0)),
        ((-1.0, 1.0, 0.0), (0.0, 0.0, 3.0), (1.5, 1.5, 1.5)),
    ],
)
# The prop itself is not exported: only its duplicates.
bpy.data.objects.remove(prop)

# The ground, an ordinary mesh node.
bpy.ops.mesh.primitive_plane_add(size=14.0, location=(0.0, 0.0, -0.5))
ground = bpy.context.object
ground.name = "Ground"
ground.data.materials.append(material("Ground", (0.6, 0.6, 0.6, 1.0)))

for directory, instances in (("glTF", True), ("glTF-Expanded", False)):
    os.makedirs(os.path.join(out, directory), exist_ok=True)
    bpy.ops.export_scene.gltf(
        filepath=os.path.join(out, directory, "InstancedProps.gltf"),
        export_format="GLTF_SEPARATE",
        export_gpu_instances=instances,
        export_materials="EXPORT",
        export_texcoords=False,
    )
