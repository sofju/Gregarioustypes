"""3D samples into samples/3d/:
- khronos/: glTF Sample Assets (PBR, skinning, morphs, Draco, KTX2, external files);
- threejs/: the three.js repository's test models (FBX, OBJ+MTL, STL, PLY, 3MF,
  3DS, COLLADA, USDZ);
- blender/: generated with the bundled Blender (--blender): .blend (with and
  without a preview, compressed), Alembic (static and animated), USD, FBX,
  OBJ and STL exports of the same scene.
Run: python -I scripts/make-3d-samples.py [--blender]
"""
import json
import os
import subprocess
import sys
import urllib.parse
import urllib.request

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OUT = os.path.join(ROOT, "samples", "3d")

KHRONOS = "https://raw.githubusercontent.com/KhronosGroup/glTF-Sample-Assets/main/Models/"
KHRONOS_FILES = [
    "DamagedHelmet/glTF-Binary/DamagedHelmet.glb",
    "Fox/glTF-Binary/Fox.glb",
    "BrainStem/glTF-Binary/BrainStem.glb",
    "AnimatedMorphCube/glTF-Binary/AnimatedMorphCube.glb",
    "Avocado/glTF-Draco/Avocado.gltf",
    "Avocado/glTF-Draco/Avocado.bin",
    "Avocado/glTF-Draco/Avocado_baseColor.png",
    "Avocado/glTF-Draco/Avocado_normal.png",
    "Avocado/glTF-Draco/Avocado_roughnessMetallic.png",
    "BoxTextured/glTF/BoxTextured.gltf",
    "BoxTextured/glTF/BoxTextured0.bin",
    "BoxTextured/glTF/CesiumLogoFlat.png",
    "StainedGlassLamp/glTF-KTX-BasisU/StainedGlassLamp.gltf",
    "StainedGlassLamp/glTF-KTX-BasisU/StainedGlassLamp.bin",
    "ToyCar/glTF-Binary/ToyCar.glb",
]

THREE = "https://raw.githubusercontent.com/mrdoob/three.js/dev/examples/models/"
THREE_FILES = [
    "fbx/Samba Dancing.fbx",
    "fbx/nurbs.fbx",
    "fbx/stanford-bunny.fbx",
    "obj/male02/male02.obj",
    "obj/male02/male02.mtl",
    "obj/male02/01_-_Default1noCulling.JPG",
    "obj/male02/male-02-1noCulling.JPG",
    "obj/male02/orig_02_-_Defaul1noCulling.JPG",
    "obj/walt/WaltHead.obj",
    "obj/walt/WaltHead.mtl",
    "stl/ascii/slotted_disk.stl",
    "stl/binary/pr2_head_pan.stl",
    "stl/binary/colored.stl",
    "ply/ascii/dolphins.ply",
    "ply/binary/Lucy100k.ply",
    "3mf/cube_gears.3mf",
    "3ds/portalgun/portalgun.3ds",
    "collada/elf/elf.dae",
    "collada/stormtrooper/stormtrooper.dae",
    "usdz/saeukkang.usdz",
]


def fetch(base, rel, dest_dir, keep_tree):
    name = rel if keep_tree else rel.split("/")[-1]
    dst = os.path.join(dest_dir, *name.split("/"))
    if os.path.exists(dst):
        return
    os.makedirs(os.path.dirname(dst), exist_ok=True)
    url = base + urllib.parse.quote(rel)
    try:
        with urllib.request.urlopen(url, timeout=120) as r, open(dst, "wb") as o:
            o.write(r.read())
    except Exception as e:
        print("skip", rel, e)


for f in KHRONOS_FILES:
    fetch(KHRONOS, f, os.path.join(OUT, "khronos"), True)
    # .gltf files: also their buffers and images.
    if f.endswith(".gltf"):
        local = os.path.join(OUT, "khronos", *f.split("/"))
        try:
            doc = json.load(open(local, encoding="utf-8"))
        except Exception:
            doc = {}
        base = f.rsplit("/", 1)[0]
        for item in doc.get("buffers", []) + doc.get("images", []):
            uri = item.get("uri", "")
            if uri and not uri.startswith("data:"):
                fetch(KHRONOS, base + "/" + urllib.parse.unquote(uri), os.path.join(OUT, "khronos"), True)
for f in THREE_FILES:
    fetch(THREE, f, os.path.join(OUT, "threejs"), True)

if "--blender" in sys.argv:
    blender = os.path.join(ROOT, "deps", "blender", "blender.exe")
    gen = os.path.join(OUT, "blender")
    os.makedirs(gen, exist_ok=True)
    script = os.path.join(ROOT, "scripts", "blender-samples.py")
    subprocess.run([blender, "-b", "--factory-startup", "-noaudio", "--python-exit-code", "1", "--python", script, "--", gen], check=True)

print("ok", sum(len(fs) for _, _, fs in os.walk(OUT)), "files")
