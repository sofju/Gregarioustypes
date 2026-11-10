"""Run inside Blender (see make-3d-samples.py): builds a small scene and saves
it as .blend (plain, compressed, without preview) and as Alembic (static and
animated), USD (usda/usdc/usdz), FBX, OBJ and STL.
"""
import math
import os
import sys

import bpy

out = sys.argv[sys.argv.index("--") + 1]
os.makedirs(out, exist_ok=True)


def ops_with(op, **kw):
    """Call an operator with only the keywords it knows (they change between versions)."""
    props = op.get_rna_type().properties.keys()
    return op(**{k: v for k, v in kw.items() if k in props})


bpy.ops.wm.read_factory_settings(use_empty=True)
scene = bpy.context.scene
scene.frame_start, scene.frame_end = 1, 48
scene.render.fps = 24


def material(name, color, metallic=0.0, rough=0.5):
    m = bpy.data.materials.new(name)
    m.use_nodes = True
    bsdf = next(n for n in m.node_tree.nodes if n.type == "BSDF_PRINCIPLED")
    bsdf.inputs["Base Color"].default_value = (*color, 1.0)
    bsdf.inputs["Metallic"].default_value = metallic
    bsdf.inputs["Roughness"].default_value = rough
    return m


bpy.ops.mesh.primitive_plane_add(size=6)
floor = bpy.context.object
floor.name = "Floor"
floor.data.materials.append(material("Floor", (0.55, 0.55, 0.58)))

bpy.ops.mesh.primitive_monkey_add(location=(0, 0, 1.2))
monkey = bpy.context.object
monkey.name = "Suzanne"
mod = monkey.modifiers.new("Subdivision", "SUBSURF")
mod.levels = 2
bpy.ops.object.shade_smooth()
monkey.data.materials.append(material("Gold", (1.0, 0.71, 0.29), metallic=1.0, rough=0.25))

bpy.ops.mesh.primitive_cube_add(size=0.8, location=(2, 0.5, 0.4))
cube = bpy.context.object
cube.name = "Cube"
cube.data.materials.append(material("Red", (0.8, 0.1, 0.08)))
# A spinning cube for the animated exports.
cube.rotation_euler = (0, 0, 0)
cube.keyframe_insert("rotation_euler", frame=1)
cube.rotation_euler = (0, 0, math.pi)
cube.keyframe_insert("rotation_euler", frame=48)
cube.location.z = 0.4
cube.keyframe_insert("location", frame=1)
cube.location.z = 1.6
cube.keyframe_insert("location", frame=24)
cube.location.z = 0.4
cube.keyframe_insert("location", frame=48)

bpy.ops.object.camera_add(location=(6, -6, 4), rotation=(math.radians(65), 0, math.radians(45)))
scene.camera = bpy.context.object
bpy.ops.object.light_add(type="SUN", location=(3, -2, 6))

# .blend files: plain with a preview (rendered from the camera), compressed,
# and one saved without a preview.
paths = bpy.context.preferences.filepaths
paths.file_preview_type = "CAMERA"
bpy.ops.wm.save_as_mainfile(filepath=os.path.join(out, "scene.blend"), compress=False)
bpy.ops.wm.save_as_mainfile(filepath=os.path.join(out, "scene-compressed.blend"), compress=True)
paths.file_preview_type = "NONE"
bpy.ops.wm.save_as_mainfile(filepath=os.path.join(out, "scene-nopreview.blend"), compress=False)

# Alembic: the whole animation, and a single frame.
ops_with(bpy.ops.wm.alembic_export, filepath=os.path.join(out, "scene-animated.abc"), start=1, end=48, evaluation_mode="RENDER")
ops_with(bpy.ops.wm.alembic_export, filepath=os.path.join(out, "scene-static.abc"), start=1, end=1, evaluation_mode="RENDER")
# USD in its three flavours.
for ext in ("usda", "usdc", "usdz"):
    ops_with(bpy.ops.wm.usd_export, filepath=os.path.join(out, "scene." + ext), export_animation=True, export_materials=True)
ops_with(bpy.ops.export_scene.fbx, filepath=os.path.join(out, "scene.fbx"), bake_anim=True, path_mode="COPY", embed_textures=True)
ops_with(bpy.ops.wm.obj_export, filepath=os.path.join(out, "scene.obj"), export_materials=True)
ops_with(bpy.ops.wm.stl_export, filepath=os.path.join(out, "suzanne.stl"), export_selected_objects=False, ascii_format=False)
print("blender samples written to", out)
