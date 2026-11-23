# egaku

*描く: to draw, to paint.*

A fast painting and image editor for desktop, written in Rust: for people
who paint, retouch photos, push pixels or animate.

![egaku with text and image objects, a group and a clipped layer](docs/screenshot.png)

Huge canvases and hundreds of layers stay fast. Text and placed images stay
editable until you decide otherwise. A grey, quiet interface keeps the
artwork the only colourful thing on screen.

```bash
cargo run --release              # empty canvas
cargo run --release -- art.ora   # open a file
```

On Linux you need the usual desktop libraries: Wayland or X11,
`libxkbcommon-x11`, and a Vulkan or GL driver.

## What it does

**Painting.** Pressure-sensitive brush and eraser with presets (pen, ink,
pencil, marker, airbrush), a stabiliser, fill with gap closing, linear and
radial gradients. True pen pressure and the pen's eraser end work on
Windows (Ink), Wayland and macOS.

**Layers.** Groups, clipping masks, twelve blend modes, opacity, alpha
lock, merge, and drag-and-drop reordering into and out of groups.

**Objects.** Text and placed images are *objects*: move, scale and rotate
them as often as you like and they never soften, because they're re-rendered
from the original every time. Text keeps its words, font, size, bold and italic,
alignment and line spacing. *Rasterize* turns either into plain pixels when
you want to paint on it.

**Selections.** Rectangle, ellipse, lasso and magic wand, with add,
subtract and intersect (Shift / Alt), feather, grow, shrink and invert.
Painting, filling, filters and Delete all stay inside the selection. Copy,
cut and paste work with other apps' clipboards too.

**Transforms.** Free transform (Ctrl+T) with handles for scale and rotation
of a layer, a group or just the selected pixels, plus flips and quarter
turns. Each transform can be smooth or nearest-neighbour.

**Photos.** Brightness/contrast, hue/saturation, levels, colour balance
(temperature, tint, vibrance), blur, sharpen, noise, threshold, posterize,
pixelate, invert and desaturate, all previewed live on the canvas. Crop,
image size, canvas size, rotate and flip. Photos with embedded colour
profiles (Display P3, Adobe RGB…) are converted to sRGB on import, so they
look as they did in the viewer they came from. Exports PNG (tagged sRGB),
JPEG and WebP.

**Pixel art.** A pixel brush (aliased, gap-free, with pixel-perfect
corners), a pixel grid at high zoom, an optional tile grid, nearest-neighbour
transforms and resizing, a palette, and exports scaled up by 2×, 4× or 8×
with crisp pixels.

**Animation.** A timeline of frames with onion skinning, playback at a
chosen frame rate, frames that can be added blank or duplicated, reordered
and deleted, and export to GIF, animated PNG, a sprite sheet or numbered
PNGs.

**Safety.** Unsaved work is autosaved in the background every minute and
offered back after a crash. Saves are atomic and never stall painting.

## Using it

Every tool shows its settings in the bar along the top, and every menu item
shows its shortcut. `F1` lists all of them. The ones worth learning first:

| Input | Action |
|---|---|
| `V` `M` `L` `W` `C` | Move, select (again for ellipse), lasso, magic wand, crop |
| `B` `E` `G` `Shift+G` `T` `I` | Brush, eraser, fill, gradient, text, eyedropper |
| `Space` drag · wheel · `R` drag | Pan · zoom · rotate the view |
| hold `Alt` · hold `Ctrl` | Eyedropper · move |
| `[` `]` · `1`…`0` | Brush size · opacity |
| `Ctrl+T` · `Enter` · `Esc` | Free transform · apply · cancel |
| `Ctrl+A` `Ctrl+D` · `Shift`/`Alt` | Select all, deselect · add/subtract |
| `Ctrl+G` · `Ctrl+Alt+G` · `Ctrl+J` | Group · clip to layer below · duplicate |
| `P` · `,` `.` | Play · previous/next frame |
| `Tab` | Hide the panels |

Dropping a `.ora` file on the window opens it; dropping an image places it
as an image object, and so does pasting one.

## Design

Each choice below borrows from something that has worked in graphics
software before.

**Sparse, copy-on-write tiles.** Every layer is a grid of 64×64 tiles, the
same layout Krita and most painting engines use. A tile nobody has painted
doesn't exist. An empty layer on a 16k canvas costs half a megabyte of
pointers instead of a gigabyte of pixels, so there's no reason to ration
layers. Tiles are reference-counted and copied only when written
(`Arc::make_mut`), so duplicating a layer or an animation frame is instant
and costs no memory until you paint on it. Selections use the same sparse
tiles with 8-bit coverage, so soft and feathered edges cost nothing extra.

**Every layer is pixels to the compositor.** Groups are a tree, but text and
image objects keep a rendered copy of themselves in ordinary tiles, so the
compositor, thumbnails, file writer and caches never need to know what kind
of layer they're looking at. A group is composited in isolation and then
blended like a layer. A clipped layer paints inside its base as if the base
were opaque, and the base's alpha, opacity and blend mode then apply to the
stack, as in Photoshop.

**Undo is a list of snapshots.** Since tiles are shared, a snapshot of the
whole document is just a tree of layer handles. Every operation can be
undone without writing any inverse code, and that includes selections,
canvas resizes and animation frames. A stroke costs only the tiles it
touched, and finding what changed between two states is a pointer
comparison. History is capped by memory use, not step count.

**Edits in progress are previews.** A stroke, a transform, a filter dialog
or a gradient drag doesn't touch the document until it's committed. Each
one plugs into the compositor as an override that replaces one layer's
tiles as they're composited, so the preview is computed lazily for just the
visible tiles and cancelling is free. While one is active, everything below
it is flattened once and cached, so working at the bottom of a 200-layer
stack costs about the same as working on one layer.

**The brush paints into a mask first.** Dabs go into a 16-bit coverage
mask. Flow sets how much each dab adds, and opacity caps the total (the
Photoshop model). The mask is applied to the layer once, when the stroke
ends. Low-flow airbrushing builds up smoothly instead of banding the way
repeated 8-bit blending does, and each stroke becomes one cheap undo step.
Pixel mode stamps aliased squares on whole pixels and drops the doubled
corner pixels of 1px lines, as Aseprite's pixel-perfect mode does.

**Resampling keeps the original.** Transforms, image objects and resizing
all go through one resampler that samples from a lazily built mip pyramid,
so shrinking a photo to a tenth of its size averages properly instead of
aliasing, and nearest-neighbour stays exact for pixel art. Objects are
always resampled from their original, so they don't accumulate blur.

**The display uses a mip pyramid too.** The composite has a box-filtered
pyramid of half-size levels, and the GPU only holds 256² textures for the
chunks in view at the level that matches the zoom. Pan, zoom, rotate and
mirror only move vertices, so navigating never recomposites anything.

**Blending happens in premultiplied sRGB.** That's what artists' eyes are
trained on (Photoshop, SAI, Procreate and Krita all default to it), and it
lets the common Normal mode run as pure integer maths. Imported images are
converted from their embedded ICC profile to sRGB with `moxcms`, and exports
are tagged as sRGB, so colours survive the round trip.

**Files are OpenRaster.** The native format is `.ora`, the open layered
format Krita, GIMP and MyPaint also read and write, so your work isn't
locked into this app. Groups are nested stacks as the spec defines them.
What ORA has no words for (clipping, text, image originals, animation
frames) is kept in `egaku:` attributes and extra files that other programs
ignore, and every layer's pixels are still written so those programs show
it correctly. Layers are cropped to their painted area and encoded in
parallel; saves are atomic and run on a background thread against a
snapshot, as do autosaves.

**The interface stays out of the way.** Tool settings live in one bar above
the canvas and change with the tool, so the side panels hold only colour,
swatches and layers. The timeline appears once there's more than one frame.
Grey chrome keeps it from skewing your colour judgement, and the one accent
colour marks what's active. Text is set in Inter and icons are Phosphor,
both bundled. Tools spring into place while a key is held (Photoshop), the
view can rotate and mirror (SAI), and 100% zoom means one canvas pixel per
physical screen pixel, even on HiDPI displays.

## Performance

`cargo run --release -p egaku-core --example bench` builds an 8192² document
with 200 painted layers, then paints on the bottom layer. On a modest
4-core cloud VM:

| | |
|---|---|
| full recomposite, 8192², 200 layers | ~290 ms |
| per-frame update while painting under 199 layers | ~0.6 ms median, ~1 ms p95 |
| committing a stroke | ~4 ms |
| undo + recomposite | ~290 ms |

The layer tree, clipping and object support cost nothing measurable here.
The same VM runs the previous flat-layer version at the same speed.

`--example make_big` writes a large test `.ora` file, and `--example demo`
writes the document in the screenshot above.

## Layout

- `crates/egaku-core`: document model, tiles, blending, brush engine,
  compositor, selections, resampling, text and image objects, filters,
  undo, animation and file formats. No GUI, unit-tested.
- `crates/egaku`: the desktop app (eframe/egui on wgpu).

## Not yet

- **Higher bit depths.** Layers are 8 bits per channel. The brush already
  accumulates in 16 bits, but 16-bit layers (for heavy photo grading
  without banding) would mean making every pixel path generic over channel
  depth.
- Pen pressure on X11 (Wayland, Windows and macOS have it).
- Layer masks and adjustment layers, curves, perspective transform.
- Per-frame timing and audio in animations.
