# Using RealityScan GTK

The parts of the app in more detail. The [README](../README.md) covers setup.

## The views and tools

### The workflow

Add images with the toolbar buttons, Ctrl+I / Ctrl+Shift+I, or by dropping files and folders on
the window. The **Run** button next to the window title (F9) does what RealityScan's Start
button does: align, a normal-detail model and a texture in one go, fitting a reconstruction
region automatically when the project has none. The pipeline buttons run the stages one at a
time: **Align** (its arrow opens the alignment settings), **Model** at preview/normal/high
detail, **Color** and **Texture** (their arrows have the preview/quality/colour-correction
variants). Live progress and RealityScan's own activity line are in the status bar; Esc aborts
the running process. Commands sent while RealityScan is busy are queued until it is idle.

Projects open and save as ordinary `.rsproj` files (Ctrl+O / Ctrl+S / Ctrl+Shift+S), with a
recent list and autosave recovery.

### Images

Thumbnails are decoded by the app and cached, so they only cost time once. Select thumbnails
to highlight their cameras in the 3D view (Ctrl+A / Ctrl+Shift+A for all / none); each image
has switches for alignment, meshing and texturing. Double-click a thumbnail (or a camera in the
3D view) to open the image at full resolution.

RealityScan treats files that share a name inside one folder, say `image_045.jpg` and
`image_045.jxl`, as layers of one image and imports a single photo per name; the app points
this out. Two folders whose files share names (two shoots both numbered `image_001…`) are fine
and are told apart throughout.

### The 3D view

Left-drag orbits around the point under the pointer (the framing centre when you start on the
background), middle/right-drag pans, the wheel zooms, and the fit button re-frames. The toolbar
toggles the sparse point cloud, the cameras, the reconstruction region box and the mesh. Click
a camera to select its image. Each model row in the sidebar expands to switches for its vertex
colours and texture, and switching models keeps the current view so detail levels can be
compared at the same zoom.

The **reconstruction region** can be fitted automatically, typed in, or dragged in the viewport:
face handles resize it, the centre handle moves it.

### Control points and scaling

Create a control point with the "+" next to *Control points*, open an image and click where
the point is, in two or more photos (drag a marker to move it, right-click to remove it). Once
a point is triangulated, every other image shows a dashed marker where that camera should see
it, projected through RealityScan's registered calibration including lens distortion. With a
model on screen, **Ctrl+click on the surface** measures the active point in three cameras that
see that spot from well-separated directions, so points can be placed from the 3D view and
refined in the images. Each point's row expands to the images it is measured in, followed
(dimmed) by the images it projects into; the eye shows or hides it in the 3D view.

Define the known distance between two points under *Distances* and run Tools › Update
Alignment to Constraints to rescale the project (components and models alike). Tools › Detect
Markers finds coded targets; ground control points import and export as CSV. Components can be
merged, exported and imported as `.rsalign` files.

### Masks

Open an image and press **Mask**: paint over what RealityScan should ignore (left drag; right
drag or the erase tool puts it back), then Apply. **AI mask** asks RealityScan to isolate the
subject in that image; the images menu does the same for the selected thumbnails, and Tools ›
Masks for every image, from the current model, or from a folder of mask images named like the
photos. RealityScan uses the masks it generates internally but can't export them in a readable
form yet, so AI masks aren't displayed.

### Mesh clean-up

Tools › Select Triangles offers RealityScan's selectors (inside/outside the region, marginal,
large, largest part, invert) followed by Remove Selected Triangles, or Cut by Region directly;
Fill Holes, Smooth, Simplify (to a triangle count or a percentage) and Clean are in the same
menu. Each filter makes a new model, as in RealityScan.

For anything the selectors can't express there is a **lasso**: the last button in the 3D view's
toolbar turns it on, then a left drag outlines the triangles to select (only the surface you can
see; Shift adds, Ctrl removes, a click clears). A bar below shows the count with *Remove*,
*Keep only*, *Invert* and *Clear* (the Delete key removes). RealityScan's command line can't
take a triangle set, so the edit is applied by writing the trimmed mesh as PLY and importing it:
the result is a new model with vertex colours kept and no texture (texture it again if needed);
the original stays until you delete it. With the sparse view shown, the lasso selects cameras,
i.e. their images in the sidebar.

### Ortho projections and export

The "+" next to *Ortho projections* renders the selected model through the top or a side of the
reconstruction region at a chosen resolution; rows can be viewed in the image view or exported
as TIFF/PNG/JPEG. **Export model** (Ctrl+E) asks for the format (OBJ, PLY, glTF, FBX, USD,
Alembic, Collada, DXF, STL, 3MF and point-cloud formats) and the vertex/texture options, then
for the file. Tools › Export has the sparse point cloud, registration, undistorted images and
XMP camera files.

### Shortcuts

F9 run the workflow · Ctrl+O open · Ctrl+S save · Ctrl+Shift+S save as · Ctrl+I add images ·
Ctrl+Shift+I add folder · Ctrl+A / Ctrl+Shift+A select all / no images · Ctrl+E export model ·
Ctrl+M re-export the mesh for display · Ctrl+L log · Ctrl+R restart RealityScan · F5 refresh ·
Esc abort · Delete remove lasso selection · Ctrl+Q quit.

## Performance notes

- **Importing images** is RealityScan's own import, which decodes every file through Wine's
  imaging layer: 72 × 45 MP JPEG XLs take about a minute (the JPEG XL codec costs ~10
  CPU-seconds per image); plain JPEGs import in seconds.
- **Models over 15 M triangles** (40 M when textured) are shown through a temporary simplified
  copy, since the viewport would otherwise stall. Ctrl+M re-exports the shown model.
- **Switching models** that haven't been shown before means RealityScan exporting them, which
  takes tens of seconds for a large textured model; once exported they come from the display
  cache (`~/.cache/realityscan-gtk/meshes`, pruned after three days).
- **AI masking speed.** RealityScan runs its masking model
  through ONNX Runtime and DirectML. Wine's `dxcore.dll` tells ONNX Runtime that no adapter
  supports D3D12, so under Wine the model silently runs on the CPU: about a minute per 45 MP
  image. `tools/dxcore/` holds a replacement (`make dxcore`, needs `winegcc` and `clang`); when
  it is present next to the installed binary (`share/realityscan-gtk/dxcore.dll`), in
  `~/.local/share/realityscan-gtk/`, or named by `REALITYSCAN_GTK_DXCORE`, the app installs it
  into the Heroic prefix with a `dxcore=native` override before launching, and masking moves to
  the GPU (about 3 s per image after a 15 s model start-up on an RTX 5090 Laptop).
  RealityScan's own UI in the same prefix benefits too. Proton runtime only.

## Troubleshooting

- **"could not find a usable NVIDIA graphics card"** in RealityScan's log: CUDA isn't reaching
  Wine. The app sets `PROTON_NVIDIA_NVCUDA=1`; for Heroic's own launches add it in the game's
  environment settings. GE-Proton 11.5 also needs a `nvidia-libs/nvapi` link inside the Proton
  directory, which the app creates on first launch.
- **The instance doesn't start / "no RealityScan found":** the log names where it looked
  (Heroic's library, `/opt/realityscan`, `~/.local/opt/realityscan`, running distroboxes).
  Make sure RealityScan was launched once from Heroic (so the prefix and login exist) and
  that `umu-run` is on the `PATH`.
- **RealityScan died / restart banner:** a headless instance is still an X client and can be
  killed by display changes; Ctrl+R restarts it and reopens the project. The RealityScan log is
  in the prefix at `drive_c/users/steamuser/AppData/Local/Temp/RealityScan.log`.
- **"Translation failed"** in the log is RealityScan's wording for a report template that
  couldn't be evaluated, usually because it was asked about a model that was still being built;
  the app retries once things are idle.
- **JPEG XL photos won't import** with Epic's package: it has no JXL decoder. Install
  RealityScan through Heroic too (the app copies the decoder across) or convert the photos.
- **Masks or textures look stale:** Ctrl+M re-exports the model for display, bypassing the
  cache. Re-alignment and component edits clear the cache on their own.

## Development notes

```bash
cargo build && cargo run      # debug build; `--release` for realistic speed
cargo test && cargo clippy --all-targets
```

| crate | role |
|---|---|
| `rs-client` | REST client, CLI command builder, Wine path translation, event receiver |
| `rs-backend` | Runtime detection (Heroic/Proton, Epic's CrossOver package, distrobox), launching/stopping instances, autosave handling |
| `rs-scene` | Reads project state back via report templates; PLY/GLB mesh readers, `.rsbox` regions, export settings XML |
| `rs-render` | OpenGL renderer: points, lines, lit and textured meshes, depth picking |
| `realityscan-gtk` | The application |

Handy tools and knobs:

- `cargo run -p rs-client --example rsctl -- status` talks to a running instance.
- `RS_HOLD=1 cargo run -p rs-backend --example launch` starts an instance, prints its URL and
  keeps it; `REALITYSCAN_GTK_ATTACH=http://127.0.0.1:PORT` then makes the app attach to it
  instead of launching one (and leaves it running when you quit). The app is single-instance;
  a second copy needs `--gapplication-app-id=org.realityscan_gtk.RealityScanGtk.test`.
- `REALITYSCAN_GTK_SCRIPT="wait:30,popup:region,…"` (or `@file`, re-run whenever the file
  changes) drives the UI for screenshots and smoke tests; the step list is in `window.rs`.
  `log:PATH` dumps the log pane.
- `REALITYSCAN_GTK_WINEDEBUG=+dxcore,…` turns Wine debug channels on for the launched instance.
- `REALITYSCAN_GTK_MAX_PREVIEW`, `_MAX_TEXTURED_PREVIEW`, `_PREVIEW_TARGET` tune the
  simplified-copy display thresholds.
- When forcing `GDK_BACKEND=x11` for screenshots, also set `GTK_IM_MODULE=simple`: GTK's IBus
  module segfaults on window destruction under XWayland.

Contributions are welcome; `cargo fmt` and a clean `cargo clippy --all-targets` are expected
(CI runs both plus the tests on Fedora), and PLAN.md is the place for findings about
RealityScan's remote interface. Epic's own documentation extracts used during development live
in `docs/realityscan-ref/` and are deliberately not committed (see its README).
