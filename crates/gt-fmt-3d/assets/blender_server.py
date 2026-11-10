# Runs inside the bundled Blender (background, factory settings, scripts in
# .blend files disabled): converts .blend files to GLB on request.
# Protocol: one JSON object per line on stdin ({"id", "src", "out"} or
# {"quit": true}); answers are stdout lines starting with "@@GT " (Blender
# prints its own messages to stdout too).
import json
import os
import sys
import time

import bpy

TAG = "@@GT "


def reply(**kw):
    sys.stdout.write(TAG + json.dumps(kw) + "\n")
    sys.stdout.flush()


def call(op, **kw):
    """Call an operator with only the options this Blender version knows."""
    known = op.get_rna_type().properties.keys()
    return op(**{k: v for k, v in kw.items() if k in known})


def summary(own_file):
    d = bpy.data
    sc = bpy.context.scene
    facts = []
    counts = {}
    for o in sc.objects:
        counts[o.type] = counts.get(o.type, 0) + 1
    names = [("MESH", "mesh", "meshes"), ("CURVE", "curve", "curves"), ("SURFACE", "surface", "surfaces"), ("META", "metaball", "metaballs"),
             ("FONT", "text", "texts"), ("CURVES", "hair", "hair"), ("POINTCLOUD", "point cloud", "point clouds"), ("VOLUME", "volume", "volumes"),
             ("GREASEPENCIL", "grease pencil", "grease pencils"), ("GPENCIL", "grease pencil", "grease pencils"), ("ARMATURE", "armature", "armatures"),
             ("LIGHT", "light", "lights"), ("CAMERA", "camera", "cameras"), ("EMPTY", "empty", "empties")]
    parts = []
    seen = set()
    for key, one, many in names:
        n = counts.get(key, 0)
        if n and one not in seen:
            seen.add(one)
            parts.append("%d %s" % (n, one if n == 1 else many))
    total = len(sc.objects)
    facts.append(["Objects", "%d (%s)" % (total, ", ".join(parts)) if parts else str(total)])
    if not own_file:
        # Imported: the scene settings are Blender's defaults, not the file's.
        return facts
    if len(d.scenes) > 1:
        facts.append(["Scenes", "%d (showing %s)" % (len(d.scenes), sc.name)])
    facts.append(["Frame range", "%d–%d at %g fps" % (sc.frame_start, sc.frame_end, sc.render.fps / sc.render.fps_base)])
    engine = {"BLENDER_EEVEE_NEXT": "EEVEE", "BLENDER_EEVEE": "EEVEE", "CYCLES": "Cycles", "BLENDER_WORKBENCH": "Workbench"}.get(sc.render.engine, sc.render.engine.title())
    facts.append(["Render", "%s, %d × %d" % (engine, sc.render.resolution_x * sc.render.resolution_percentage // 100, sc.render.resolution_y * sc.render.resolution_percentage // 100)])
    if d.materials:
        facts.append(["Materials", str(len([m for m in d.materials if m.users]))])
    images = [i for i in d.images if i.users and i.source in ("FILE", "TILED")]
    if images:
        missing = [i for i in images if not i.packed_file and not os.path.exists(bpy.path.abspath(i.filepath))]
        facts.append(["Images", "%d%s" % (len(images), (" (%d missing)" % len(missing)) if missing else "")])
    libs = len(d.libraries)
    if libs:
        facts.append(["Linked libraries", str(libs)])
    return facts


def importer(kind):
    ops = bpy.ops
    return {
        "usd": (ops.wm.usd_import, {}),
        "abc": (ops.wm.alembic_import, {"as_background_job": False}),
        "fbx": (ops.import_scene.fbx, {}),
        "obj": (ops.wm.obj_import, {}),
        "ply": (ops.wm.ply_import, {}),
        "stl": (ops.wm.stl_import, {}),
        "gltf": (ops.import_scene.gltf, {}),
    }[kind]


def convert(src, out, kind):
    t = time.time()
    if kind == "blend":
        bpy.ops.wm.open_mainfile(filepath=src, load_ui=False, use_scripts=False)
    else:
        bpy.ops.wm.read_factory_settings(use_empty=True)
        op, extra = importer(kind)
        call(op, filepath=src, **extra)
        if not bpy.context.scene.objects:
            raise RuntimeError("Blender found nothing to show in this file")
    t_open = time.time() - t
    facts = summary(kind == "blend")
    call(bpy.ops.export_scene.gltf, filepath=out, check_existing=False, export_format="GLB", use_visible=True, use_renderable=False,
         export_apply=True, export_animations=True, export_skins=True, export_morph=True, export_cameras=False, export_lights=False,
         export_extras=False, export_yup=True, export_image_format="AUTO", export_draco_mesh_compression_enable=False,
         will_save_settings=False, export_import_convert_lighting_mode="SPEC")
    ms_total = int((time.time() - t) * 1000)
    # Drop the file's data before the next request.
    bpy.ops.wm.read_factory_settings(use_empty=True)
    return facts, int(t_open * 1000), ms_total


def main():
    bpy.ops.wm.read_factory_settings(use_empty=True)
    reply(ready=True, version=bpy.app.version_string)
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            req = json.loads(line)
        except Exception:
            continue
        if req.get("quit"):
            break
        try:
            facts, ms_open, ms = convert(req["src"], req["out"], req.get("kind", "blend"))
            reply(id=req.get("id"), ok=True, facts=facts, ms_open=ms_open, ms=ms)
        except Exception as e:
            reply(id=req.get("id"), ok=False, error=str(e).strip().splitlines()[-1] if str(e).strip() else type(e).__name__)


main()
