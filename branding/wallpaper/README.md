# Slate

The wallpaper for Nobara and Tessel: a few broad layers of stone with flowing
edges, each resting on the one before, under one soft light. The stone is black
and dark grey in front, then tanzanite, a dark blue with a hint of purple, with
blue zircon at the back. Tanzanite and blue zircon are the colors of the Nobara
logo.

Nothing else happens in the picture. There is no pattern, no texture and
nothing left to chance, and it carries no logo or text, so both projects can
use it as it is.

There is a dark version and a light one of the same scene.

## Files

| File | What it is |
|---|---|
| `slate.py` | Builds the whole scene in Blender and renders it. The source of everything else. |
| `slate.blend`, `slate-light.blend` | The two scenes as Blender projects, saved by the script, for editing by hand. |
| `renders/slate-3840x2160.png` | Dark, 16:9, 4K |
| `renders/slate-3840x2400.png` | Dark, 16:10 |
| `renders/slate-7680x2160.png` | Dark, 32:9 |
| `renders/slate-light-<size>.png` | The light version in the same three sizes |
| `previews/` | Both versions, small. |

Each size is rendered on its own, not cropped from another. The camera keeps
its vertical field of view, so a wider frame shows more of the layers at the
sides and all three share the same center. Smaller screens of the same shape
can use these scaled down (1920x1080, 2560x1600, 5120x1440 and so on).

## Rendering

    blender -b --factory-startup -P slate.py -- --render out.png --size 3840x2160
    blender -b --factory-startup -P slate.py -- --light --render out-light.png
    blender -b --factory-startup -P slate.py -- --save slate.blend

`--size` takes any width and height. `--preview` with a small size and
`--samples 256` gives a look in a few seconds. `--cpu` renders without a GPU.
The script picks an AMD GPU through HIP when Blender offers one.

Made with Blender 5.2 and Cycles.

## Changing it

Everything is a named value near the top of `slate.py` or in the function it
belongs to:

- **Colors**: `TONES`. Each entry gives the stone at night and by day: the
  nearest layer, the farthest layer, the haze the far layers melt into, and
  then the layers in between. `gems` is the wallpaper; `tanzanite` and `sand`
  are single-color versions to try with `--tone`.
- **The shape of the layers**: `EDGES` holds the front edge of each one, as a
  distance from the camera plus waves along it. `STEP` is how much higher each
  layer lies and `ROUND` how round its edge is.
- **Light**: one broad lamp in `build_scene()`, with `KEY_COLOR` and
  `KEY_POWER`. What differs between night and day besides the stone is in
  `THEMES`.
- **Finish of the stone**: `ROUGHNESS` and `SPECULAR`.
- **Framing and focus**: `CAMERA` and `TARGET`. Everything is in focus;
  `F_STOP` set to a number softens what is far in front and far behind.

## Notes

- This Blender build has no denoiser, so the script renders with enough samples
  to be clean without one.
- Output is 8-bit PNG with dithering, which keeps the soft gradients from
  banding.
