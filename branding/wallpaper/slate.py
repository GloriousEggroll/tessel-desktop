"""Build and render the "Slate" wallpaper for Nobara and Tessel.

A few broad layers of stone with flowing edges, each resting on the one before,
under one soft light. The stone is black and dark grey in front, then
tanzanite, with blue zircon at the back: the colors of the Nobara logo. Nothing
else happens in the picture: no pattern, no texture, no logo, nothing left to
chance.

    blender -b --factory-startup -P slate.py -- [options]

    --save FILE.blend     save the scene as a project file
    --render FILE.png     render a still
    --size WxH            output size (default 3840x2160)
    --samples N           Cycles samples (default 1024)
    --cpu                 render on the CPU instead of the GPU
    --preview             coarse settings for a quick look
    --light               the daytime version (pale stone)
    --tone NAME           the colors of the stone (default gems)

The camera keeps its vertical field of view, so a wider size shows more of the
layers at the sides and every aspect ratio shares the same center.
"""
import argparse
import math
import sys

import bmesh
import bpy
from mathutils import Vector

# ---- palette (sRGB hex) -------------------------------------------------------
# The stone, at night and by day: (nearest layer, farthest layer, haze), then
# any layers between the nearest and the farthest, in order.
TONES = {
    # The wallpaper: black and dark grey in front, then tanzanite, and blue
    # zircon at the back.
    "gems": (("#131417", "#2c8cc2", "#090a10", "#1d1e24", "#22265f", "#2b3496"),
             ("#eeeff3", "#86c8ea", "#eef3f7", "#dcdde5", "#a9b1ec", "#8a96e6")),
    # Others to try: one color, deep in front and lighter toward the back.
    "tanzanite": (("#1c1a32", "#5446c2", "#100e24"), ("#e9e7f7", "#8f84e2", "#efedfa")),
    "sand": (("#2d2b29", "#a1978b", "#211f1d"), ("#efebe6", "#c3b8aa", "#f3f0ec")),
}
TONE = TONES["gems"][0]
NEAR, FAR, HAZE = range(3)

# What differs between the night and the day version besides the stone.
THEMES = {
    "dark": {
        "ambient": "#2a2927",       # what the sky lends the stone
        "bloom": (0.6, 0.5),        # threshold, strength
    },
    "light": {
        "ambient": "#cfcdca",
        "bloom": (1.25, 0.25),
    },
}
THEME = THEMES["dark"]
ROUGHNESS = 0.62
SPECULAR = 0.35
KEY_COLOR, KEY_POWER = "#ffffff", 41000


def srgb(hex_color, alpha=1.0):
    """sRGB hex -> scene-linear RGBA."""
    value = hex_color.lstrip("#")
    channels = [int(value[i:i + 2], 16) / 255.0 for i in (0, 2, 4)]
    linear = [c / 12.92 if c <= 0.04045 else ((c + 0.055) / 1.055) ** 2.4 for c in channels]
    return (*linear, alpha)


def mix_hex(a, b, t):
    pa, pb = [int(a[i:i + 2], 16) for i in (1, 3, 5)], [int(b[i:i + 2], 16) for i in (1, 3, 5)]
    return "#%02x%02x%02x" % tuple(round(pa[i] + (pb[i] - pa[i]) * t) for i in range(3))


# ---- the layers ----------------------------------------------------------------
CAMERA = Vector((0.0, -15.0, 16.0))
TARGET = Vector((0.0, 8.5, 0.0))
F_STOP = 0                  # 0: everything sharp. A number (4.5, say) softens what is far in front and far behind
X_HALF = 190.0              # wide enough for a 32:9 frame
Y_NEAR, Y_FAR = -30.0, 110.0
STEP = 0.8                  # how much higher each layer lies
ROUND = 0.06                # radius of a layer's front edge
COLOR_LAYERS = 4            # the stone reaches its far color at this layer
FADE = (15.0, 61.0)         # the layers melt into the haze between these distances into the scene

# Front edge of each layer above the ground: y = base + waves along x,
# each wave as (height, frequency, phase).
EDGES = (
    (0.5, ((2.2, 0.085, 0.3), (0.6, 0.19, 1.0))),
    (9.5, ((3.0, 0.075, 1.4), (0.8, 0.17, 0.5))),
    (20.0, ((3.6, 0.066, 2.9),)),
    (33.0, ((4.2, 0.058, 4.3),)),
    (49.0, ((4.8, 0.050, 5.5),)),
    (68.0, ((5.5, 0.045, 0.9),)),
)


def edge(layer, x):
    base, waves = EDGES[layer - 1]
    return base + sum(a * math.sin(x * f + p) for a, f, p in waves)


def build_layer(collection, layer, material):
    """Layer 0 is the ground. The others start at their edge, with a rounded
    lip and a face going down to the layer below."""
    mesh = bpy.data.meshes.new(f"Layer {layer}")
    bm = bmesh.new()
    z = STEP * layer
    step = 0.5
    columns = int(2 * X_HALF / step) + 1
    previous = None
    for c in range(columns):
        x = -X_HALF + c * step
        if layer == 0:
            profile = [(Y_FAR, z), (Y_NEAR, z)]
        else:
            e = edge(layer, x)
            profile = [(Y_FAR, z), (e + 24.0, z), (e + 8.0, z), (e + ROUND, z)]
            for i in range(1, 13):
                angle = math.pi / 2 * i / 12
                profile.append((e + ROUND - ROUND * math.sin(angle), z - ROUND + ROUND * math.cos(angle)))
            profile.append((e, z - STEP - 1.6))
        column = [bm.verts.new((x, y, height)) for y, height in profile]
        if previous:
            for i in range(len(column) - 1):
                bm.faces.new((previous[i], previous[i + 1], column[i + 1], column[i]))
        previous = column
    bm.normal_update()
    bm.to_mesh(mesh)
    bm.free()
    for polygon in mesh.polygons:
        polygon.use_smooth = True
    mesh.materials.append(material)
    obj = bpy.data.objects.new(mesh.name, mesh)
    collection.objects.link(obj)
    return obj


# ---- materials -----------------------------------------------------------------
def depth_fade(tree, shader_socket, start, end):
    """Mix a surface into the haze with its distance into the scene. It goes
    by how far back a point lies, not by how far it is from the camera, so a
    layer keeps its color to both ends of the widest frame."""
    nodes, links = tree.nodes, tree.links
    geometry = nodes.new("ShaderNodeNewGeometry")
    split = nodes.new("ShaderNodeSeparateXYZ")
    links.new(geometry.outputs["Position"], split.inputs["Vector"])
    ramp = nodes.new("ShaderNodeMapRange")
    ramp.inputs["From Min"].default_value = start
    ramp.inputs["From Max"].default_value = end
    ramp.interpolation_type = "SMOOTHERSTEP"
    links.new(split.outputs["Y"], ramp.inputs["Value"])
    fog = nodes.new("ShaderNodeEmission")
    fog.inputs["Color"].default_value = srgb(TONE[HAZE])
    mix = nodes.new("ShaderNodeMixShader")
    links.new(ramp.outputs["Result"], mix.inputs["Fac"])
    links.new(shader_socket, mix.inputs[1])
    links.new(fog.outputs["Emission"], mix.inputs[2])
    return mix.outputs["Shader"]


def stone_material(depth=0.0):
    """Stone for one layer; `depth` runs from 0 (nearest) to 1 (farthest)."""
    material = bpy.data.materials.new(f"Stone {depth:.2f}")
    material.use_nodes = True
    tree = material.node_tree
    bsdf = tree.nodes["Principled BSDF"]
    stops = [TONE[NEAR], *TONE[HAZE + 1:], TONE[FAR]]
    place = min(depth, 0.9999) * (len(stops) - 1)
    color = mix_hex(stops[int(place)], stops[int(place) + 1], place - int(place))
    bsdf.inputs["Base Color"].default_value = srgb(color)
    bsdf.inputs["Metallic"].default_value = 0.0
    bsdf.inputs["Roughness"].default_value = ROUGHNESS
    bsdf.inputs["Coat Weight"].default_value = 0.0
    bsdf.inputs["Specular IOR Level"].default_value = SPECULAR
    tree.links.new(depth_fade(tree, bsdf.outputs["BSDF"], *FADE), tree.nodes["Material Output"].inputs["Surface"])
    return material


def build_world():
    world = bpy.data.worlds.new("Backdrop")
    world.use_nodes = True
    tree = world.node_tree
    nodes, links = tree.nodes, tree.links
    nodes.clear()
    # The camera sees the haze; the stone is lit by a sky of its own, so the
    # haze can be as dark or as bright as the picture needs.
    path = nodes.new("ShaderNodeLightPath")
    color = nodes.new("ShaderNodeMix")
    color.data_type = "RGBA"
    links.new(path.outputs["Is Camera Ray"], color.inputs["Factor"])
    color.inputs["A"].default_value = srgb(THEME["ambient"])
    color.inputs["B"].default_value = srgb(TONE[HAZE])
    background = nodes.new("ShaderNodeBackground")
    background.inputs["Strength"].default_value = 1.0
    links.new(color.outputs["Result"], background.inputs["Color"])
    links.new(background.outputs["Background"], nodes.new("ShaderNodeOutputWorld").inputs["Surface"])
    return world


def area_light(collection, name, location, target, color, power, size):
    data = bpy.data.lights.new(name, "AREA")
    data.shape = "RECTANGLE"
    data.size, data.size_y = size
    data.energy = power
    data.color = srgb(color)[:3]
    obj = bpy.data.objects.new(name, data)
    obj.location = location
    obj.rotation_euler = (Vector(target) - Vector(location)).to_track_quat("-Z", "Y").to_euler()
    collection.objects.link(obj)
    return obj


# ---- scene -----------------------------------------------------------------------
def build_scene():
    bpy.ops.wm.read_factory_settings(use_empty=True)
    scene = bpy.context.scene
    scene.name = "Slate"
    collection = scene.collection
    scene.world = build_world()
    for layer in range(len(EDGES) + 1):
        build_layer(collection, layer, stone_material(min(1.0, layer / COLOR_LAYERS)))

    # One broad light from behind and to the left: it grades each layer from
    # light to dark and lays a soft shadow under every edge.
    area_light(collection, "Key", (-46, 50, 40), (0, 12, 0), KEY_COLOR, KEY_POWER, (56, 32))

    camera_data = bpy.data.cameras.new("Camera")
    camera_data.lens = 32
    camera_data.sensor_fit = "VERTICAL"
    camera_data.sensor_height = 24
    camera_data.clip_end = 500
    if F_STOP:
        camera_data.dof.use_dof = True
        camera_data.dof.focus_distance = (TARGET + Vector((0, 2.0, 0)) - CAMERA).length
        camera_data.dof.aperture_fstop = F_STOP
    camera = bpy.data.objects.new("Camera", camera_data)
    camera.location = CAMERA
    camera.rotation_euler = (TARGET - CAMERA).to_track_quat("-Z", "Y").to_euler()
    collection.objects.link(camera)
    scene.camera = camera
    return scene


def build_compositor(scene):
    """A soft bloom where the stone is brightest, as a lens would give it."""
    group = bpy.data.node_groups.new("Finish", "CompositorNodeTree")
    scene.compositing_node_group = group
    group.interface.new_socket("Image", in_out="OUTPUT", socket_type="NodeSocketColor")
    nodes, links = group.nodes, group.links
    layers = nodes.new("CompositorNodeRLayers")
    glare = nodes.new("CompositorNodeGlare")
    for kind in ("Bloom", "Fog Glow"):
        try:
            glare.inputs["Type"].default_value = kind
            break
        except TypeError:
            continue
    glare.inputs["Quality"].default_value = "High"
    glare.inputs["Threshold"].default_value, glare.inputs["Strength"].default_value = THEME["bloom"]
    glare.inputs["Smoothness"].default_value = 0.5
    glare.inputs["Saturation"].default_value = 1.0
    glare.inputs["Size"].default_value = 0.62
    out = nodes.new("NodeGroupOutput")
    links.new(layers.outputs["Image"], glare.inputs["Image"])
    links.new(glare.outputs["Image"], out.inputs["Image"])


def configure_render(scene, width, height_px, samples, use_gpu, preview):
    render = scene.render
    render.engine = "CYCLES"
    render.resolution_x, render.resolution_y = width, height_px
    render.resolution_percentage = 100
    render.image_settings.file_format = "PNG"
    render.image_settings.color_mode = "RGB"
    render.image_settings.color_depth = "8"
    render.image_settings.compression = 60
    # Soft gradients band in 8 bits without it.
    render.dither_intensity = 1.6
    cycles = scene.cycles
    cycles.samples = samples
    cycles.use_adaptive_sampling = True
    cycles.adaptive_threshold = 0.02 if preview else 0.004
    cycles.use_denoising = False
    cycles.max_bounces = 6
    cycles.glossy_bounces = 4
    cycles.diffuse_bounces = 3
    cycles.caustics_reflective = False
    cycles.caustics_refractive = False
    cycles.sample_clamp_indirect = 6.0
    cycles.pixel_filter_type = "BLACKMAN_HARRIS"
    cycles.filter_width = 1.6
    if use_gpu:
        preferences = bpy.context.preferences.addons["cycles"].preferences
        try:
            preferences.compute_device_type = "HIP"
            preferences.get_devices()
            for device in preferences.devices:
                device.use = device.type == "HIP" and "Radeon" in device.name
            if any(device.use for device in preferences.devices):
                cycles.device = "GPU"
        except Exception as error:  # no GPU backend in this build
            print("GPU rendering unavailable:", error)


def main():
    argv = sys.argv[sys.argv.index("--") + 1:] if "--" in sys.argv else []
    parser = argparse.ArgumentParser(prog="slate.py")
    parser.add_argument("--save")
    parser.add_argument("--render")
    parser.add_argument("--size", default="3840x2160")
    parser.add_argument("--samples", type=int, default=1024)
    parser.add_argument("--cpu", action="store_true")
    parser.add_argument("--preview", action="store_true")
    parser.add_argument("--light", action="store_true", help="the daytime version")
    parser.add_argument("--tone", choices=sorted(TONES), default="gems", help="the colors of the stone")
    args = parser.parse_args(argv)
    global THEME, TONE
    THEME = THEMES["light" if args.light else "dark"]
    TONE = TONES[args.tone][1 if args.light else 0]
    width, height_px = (int(v) for v in args.size.lower().split("x"))
    scene = build_scene()
    build_compositor(scene)
    configure_render(scene, width, height_px, args.samples, not args.cpu, args.preview)
    if args.save:
        bpy.ops.wm.save_as_mainfile(filepath=args.save, compress=True)
    if args.render:
        scene.render.filepath = args.render
        bpy.ops.render.render(write_still=True)


if __name__ == "__main__":
    main()
