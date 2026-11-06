# realityscan-gtk — plan

A native Linux (GTK4 + libadwaita, Rust) front-end for Epic's RealityScan, aiming for
parity with the Windows UI. RealityScan itself still does all processing, running
headless under Wine; this app replaces only the user interface.

Decisions: Rust + gtk4-rs/libadwaita, aim for full parity, separate repo (this one).
Status: Phase 0 done; Phases 1–3 partly done (see the ticks below). Day 2 (2026-10-13) added
textured display, component/model lists, image switches, the reconstruction-region panel, mesh
tools, export, recent projects and drag-and-drop.

---

## 1. Why

RealityScan 2.2 runs under Wine/Proton, but its UI is a poor fit there:

- The main UI ignores Windows DPI and only scales partially with its own Zoom setting
  (the ribbon never scales).
- Popups are embedded IE (MSHTML) pages that Wine renders wrongly. They're fixed for now
  by patching `RealityScan.exe` (see the Wine prefix's `jxl_wicfix_src/rsdlgfx/README.txt`),
  but a Heroic "Verify" or an update undoes that.
- Popups are override-redirect X windows that never get keyboard focus under XWayland.
- Wine-Wayland mode can't show the window at all at 144 DPI.

Epic's own Linux build (see §3.4) says the same: run the CLI or remote-command server, and
don't use the desktop UI. A native UI avoids all of this and integrates with GNOME (portals,
file chooser, notifications, fractional scaling, dark mode).

---

## 2. Architecture

```
┌──────────────────────────── realityscan-gtk (native, Rust) ─────────────────────────────┐
│  UI (gtk4-rs / libadwaita)                                                               │
│    ├─ Project / image browser        ├─ Inspector (settings forms)   ├─ Task/progress   │
│    ├─ 2D image view (control points, masks)                                              │
│    └─ 3D viewport (GtkGLArea: sparse cloud, cameras, mesh, recon region, gizmos)          │
│  App state (scene model mirrored from RealityScan via templates)                         │
│  rs-client ── POST /cmds, /template, GET /status ──┐   ◄── event listener (POST /events)  │
│  rs-backend: launches/stops the RealityScan instance under Wine, path translation        │
└────────────────────────────────────────────────────┼─────────────────────────────────────┘
                              HTTP 127.0.0.1:<port>  │  events → 127.0.0.1:<port2>
                          ┌──────────────────────────▼─────────────────────────┐
                          │ RealityScan.exe -silent … -headless                 │
                          │   + Remote Command plugin (REST server)            │
                          │   one instance per open project (max 4)            │
                          └────────────────────────────────────────────────────┘
```

### Workspace layout (Cargo workspace)

| crate | role |
|---|---|
| `rs-client` | Async REST client for the Remote Command plugin (`/cmds`, `/status`, `/template`, `/abort`, `/pause`, `/resume`, `/stop`, `/health-check`), a typed command builder covering the ~200 CLI commands (quoting, path translation), and the notification receiver (small local HTTP server parsing event JSON). No GTK dependency, so it's testable headless. |
| `rs-backend` | Finds the RealityScan install and Wine runtime (Heroic/umu/GE-Proton today; Epic's Linux build later), writes the start-up `.rscmd`, launches and stops instances, health checks, log capture, stale-autosave handling, Linux ↔ `Z:\` path translation. |
| `rs-scene` | Scene model: images, calibration groups, components, cameras, control points, constraints, models, textures, ortho projections, classification. Built from **template queries** (§3.3) plus parsers for exports (registration CSV/XMP, sparse cloud, PLY/OBJ/glTF). |
| `rs-render` | OpenGL renderer (glow on GtkGLArea): point clouds, camera frusta, meshes (vertex colour + textures), recon-region box with gizmos, picking, selection. |
| `realityscan-gtk` | The application: windows, views, inspector panels, actions, keyboard shortcuts, settings. |

### Key technical choices

- **gtk4-rs 0.11 + libadwaita 0.9**, plain composite templates (Blueprint or `.ui`), no relm4,
  for full control over custom widgets (viewport, image view).
- **Async:** `tokio` for HTTP, bridged to the GLib main loop with channels
  (`async-channel` + `glib::spawn_future_local`).
- **HTTP:** `reqwest` with TLS disabled (localhost only) for the client; `axum` (or bare
  `hyper`) for the event receiver.
- **3D:** `glow` 0.18 on `GtkGLArea` (GL 3.3 core), `glam` for maths. Meshes can be large
  (tens of millions of triangles), so plan for LOD (`exportLod`) and streaming from day one.
- **Images:** `image` + `jxl-oxide` for JPEG XL; thumbnails cached on disk.

---

## 3. Backend: the Remote Command plugin (verified 2026-10-12)

### 3.1 What it is

RealityScan 2.1+ ships `Plugins/RealityScan.RemoteCommandPlugin/` (`.rsplugin` + `.dll`).
Once registered, it adds commands that start a REST or gRPC server inside the running
instance, plus push notifications. Epic documents it as the supported way to drive RealityScan
on Linux. Samples, OpenAPI yaml and `.proto` files are in Epic's
[samples zip](https://dev.epicgames.com/documentation/realityscan/samples-remote-command-plugin).
Copies of the relevant specs live in `docs/realityscan-ref/`.

### 3.2 Launch recipe (tested under Heroic's GE-Proton 11-5 via umu)

The plugin isn't persistently registered: `-registerPlugin` only lasts for the running
instance, and the command line is validated before anything runs. Passing
`-RsRemoteStartREST` on the same command line fails with "unknown command". Epic's samples
register first and then use `-delegateTo` from a second process. Under umu, that second
process blocks because Proton's `waitforexitandrun` verb waits for the running prefix.
What works in **one launch** is an `.rscmd` file, since `-execRSCMD` is read when it runs:

```
RealityScan.exe -silent C:\…\realityscan-gtk\crash -headless -setInstanceName <name>
                -registerPlugin <…\RealityScan.RemoteCommandPlugin.rsplugin>
                -execRSCMD C:\…\realityscan-gtk\start.rscmd
start.rscmd:    -RsRemoteStartREST http://127.0.0.1:<port>
                -RsRemoteStartNotifyREST http://127.0.0.1:<eventport>/events
```

- `-silent <dir>` is essential: any dialog (errors, "recover autosave?") otherwise pulls
  headless mode back into the UI and stalls the command queue. With `-silent`, a failing
  command produced no dialog and reported the error through `/status` and events.
- A stale autosave (`HKCU\Software\EpicGames.RealityScan\RealityScan\Auto Save`, files under
  `%TEMP%\RSTemp\{guid}`) shows a recovery prompt on start-up without `-silent`. With `-silent`
  the prompt is suppressed, the autosave is ignored (empty project) and the registry entry stays.
  `rs-backend::autosave` lists and clears entries so the UI can offer recovery itself. Killing an
  instance always leaves one behind, so shut down by sending `-quit` and only kill as a last resort.
- Even headless, RealityScan is an X11 client. An instance died after ~25 min with
  `XIO: fatal IO error 110 (Connection timed out) on X server ":0"` (XWayland). The backend must
  health-check and relaunch, and should consider giving instances their own X server (Xvfb or
  `xwayland-run`) so they don't depend on the desktop session.
- The server binds exactly the given address (127.0.0.1). Routes have **no `/v1` prefix**,
  despite the yaml's `servers:` URL.
- umu/Proton's default verb `waitforexitandrun` blocks while anything else runs in the prefix,
  so a second instance (or a `-delegateTo` call) never starts. `PROTON_VERB=run` fixes that: a
  second instance came up in 10 s next to the first. After a clean `-quit` the instance's
  `umu-run` can linger while other instances keep the shared wineserver alive; wait for the REST
  server to disappear, then kill the launcher process (never `wineserver -k`, which would take
  every instance down). A clean `-quit` leaves no autosave entry.
- Kill the **whole process tree**, never just `umu-run`: the sandbox tree (`bwrap` →
  `pv-adverb` → Proton → Wine) survives its parent, and a Proton still waiting for the prefix
  then launched RealityScan minutes later with nobody listening. Every layer starts its own
  session, so process-group/session kills don't reach it either. `rs-backend::proc_tree` lists
  descendants from `/proc` and SIGKILLs them all (on timeout, kill and drop).
- After `-quit`, wait for **connection refused** (`Client::is_gone`), not for a failed health
  check: a single failed request can happen while RealityScan is still alive.
- **CUDA needs `PROTON_NVIDIA_NVCUDA=1`.** Without it GE-Proton installs a 35 KB `nvcuda.dll`
  stub with a `disabled` override and RealityScan reports "could not find a usable NVIDIA
  graphics card" (Vulkan/DXVK works regardless, which is misleading). With the flag, GE-Proton
  copies its real `nvidia-libs/nvcuda/nvcuda.dll.so` (wine-nvcuda) and depth maps run on the GPU.
  GE-Proton 11-5's script then also wants `nvidia-libs/nvapi/`, which it doesn't ship, and
  crashes; `rs-backend` symlinks it to the shipped `nvapi/` (dxvk-nvapi) before launching.
  Proton-CachyOS ships the directory. Don't set `PROTON_NVIDIA_LIBS`.
  The same flag is needed in Heroic's environment settings for RealityScan's own UI.
- **Never send a command while RealityScan is busy.** A `POST /cmds` during a running process
  makes RealityScan ask "Do you want to abort processing?", and `-silent` answers for it: a
  queued `-save` aborted a 12-minute reconstruction at 90 %. The app holds commands that arrive
  while `/status` is busy and sends them once it is idle (this also covers the brief busy
  period right after start-up, which used to swallow a project passed on the command line).
- CUDA confirmed on both GE-Proton 11 and Proton-CachyOS 11 (depth maps and texturing ran on
  the GPU). Popup defaults can be preset
  with `-set "PUS-1-<id>=<1..3>"` (the log prints the id when it suppresses one).
- `-save` with no path saves the open project in place (fast, no reload). `-save <path>` is a
  "save as"; pointing it at the file that is already open made RealityScan close and reopen the
  project, which would drop unsaved work, so the app only ever uses the bare form for that case.
- Two Proton builds can't share a prefix at the same time: when Heroic's RealityScan entry was
  switched to Proton-CachyOS while a GE-Proton instance was still running, the new launch died
  with "wine client error: version mismatch". The launcher follows Heroic's current Proton; the
  prefix is migrated by Proton on first launch and our DLL fixes survive that.
- Saved projects store image folders *relative* to the project, and the number of `..` levels
  RealityScan writes is unreliable (3 in one save, 9 in a later save of the same images).
  `Scene::image_local_path` drops the `..` prefix and searches the project folder's ancestors
  for the file instead.
- Project loads (`-load`) run in the background without `isBackgroundBusy`; a 72-image project
  took 111 s. Queued commands do wait for it, but state queries made meanwhile see a
  half-loaded project.
- **Notifications:** `-RsRemoteStartNotifyREST` does not reliably replace an existing target.
  When attaching to an instance that already notified someone else, nothing arrived until the
  backend sent `-RsRemoteStopNotifyREST` first (now always done in one sequence with the start).
- **`-selectImage` matches the path exactly as the project stores it** (the `dir+name+ext` the
  state template reports, relative `..\` prefix included). Absolute paths, bare file names and
  the documented regular expressions all selected nothing. `-selectAllImages` works, and
  `-enableAlignment/-enableMeshing/-enableTexturingAndColoring true|false` act on the selection.
- **Model export settings XML** (`exportSelectedModel <file> <params.xml>`): the root is
  `<ModelExport …><Header magic="5786949" version="6"/><Layer0 …/></ModelExport>`; the attribute
  names are those RealityScan writes into the `.rsInfo` file next to any export. The format comes
  from `formatAndVersionUID="glb 000 "` (likewise `obj`, `ply`), **not** from the file name (a
  minimal XML without it produced OBJ for a `.glb` request), and textures are only written when
  `exportedLayerCount="1"` plus the `Layer0` element are present. `embedTextures="1"` puts the PNG
  inside the GLB. `rs_scene::export::ModelExportOptions::to_xml` reproduces the full set.
- **Exporting an untextured model with `exportTexturing="1"` crashes RealityScan** (no error,
  the process just dies and leaves 0-byte files). The backend forces the texture options off
  for models whose `modelIsTextured` is false. `simplify` creates a *new* model ("Model N+1",
  selected) rather than changing the selected one, and the copy is untextured.
- **GLB export** of a 5 M-triangle textured model: 18–40 s, 141 MB + an 8192² PNG next to it
  (`<stem>_u0_v0_diffuse.png`), `COLOR_0` as float RGB, `TEXCOORD_0`, no normals by default.
  `rs_scene::gltf` reads it in ~2 s. The app exports GLB for textured models and PLY otherwise.
- **Reconstruction region** round-trips through `.rsbox` XML (`yawPitchRoll`, `widthHeightDepth`,
  `<centre>`, `Residual R/t/s ownerId`, `Header magic="5395016" version="2"`); exports don't bump
  the change counter, so the app re-exports it once per project change.
- After `setReconstructionRegion`, RealityScan's own `exportReconstructionRegion` writes
  `widthHeightDepth` as a child element instead of an attribute; the parser accepts both.
- `/status` error messages contain unescaped Windows paths (`Z:\tmp\…`), so the JSON sanitiser
  also doubles stray backslashes.
- **A failing `/template` evaluation is a failed process**: it sets the status error and emits
  `SEQUENCE_EXECUTION_STATE_UPDATE`, so "refresh on sequence finished" + "refresh evaluates a
  failing template" is an infinite loop (seen at ~6 refreshes/s). The backend now debounces
  event-driven refreshes and stops evaluating the model-info template until the project changes
  once it has failed. `$ExportTextureInfo` fails for untextured models; guard it with
  `$If(modelIsTextured, …)`, which is evaluated lazily.
- **Sequence events say nothing about *which* sequence finished** (ids are mostly zeros), and
  RealityScan reports a sequence finished slightly before its effect shows in templates (a
  deleted model was still listed 0.4 s later) and can flash busy for a moment afterwards. The
  backend therefore judges its own exports only by their `-tag`s, treats state read within
  1.5 s of a command as unsettled, retries a deferred display export on the next idle tick,
  and marks exports whose tag never arrived while idle as failed (no retry until the project
  changes).
- **Control points have no "add" command.** Measurements are imported from a CSV
  (`importControlPointsMeasurements`, default format `Image, Point, X, Y`; the image column
  is the file name with extension OR the full stored Windows path — use the path, a bare name
  resolves to the first input with that name when folders share names; a new point name
  creates the point). Pixel coordinates
  round-trip exactly (origin top-left, full-resolution pixels). `$ExportControlPoints` reports a
  one-measurement point as aligned with position `-nan(ind)` (invalid JSON; the parser maps it
  to null). Measurements report the image by its stored project path, matching the state
  template's `dir+name+ext`. Two measurements triangulate immediately; `-defineDistance a b d
  [name]` then `-update` rescales everything.
- `$Using(ModelExportFunctionSet)` + `$ExportModels`/`$ExportTextureInfo` give
  `modelIsColored`, `modelIsTextured`, texture count and resolution for the selected component;
  the app evaluates that template only when the selected component has models
  (`templates/models.tpl`); no model needs to be selected.

### 3.3 API (as observed)

| call | notes |
|---|---|
| `GET /health-check` | 200 when up |
| `GET /status` | `{processId, progress, timeTotal, timeEstimation, isSequencerEmpty, isMasterBusy, isBackgroundBusy, changeCounter, errorCode, errorMessage}` |
| `POST /cmds` (text/plain) | a normal CLI command string, e.g. `-newScene -add "Z:\path\img.jxl" -tag ADDED`; returns `202 {"sequenceId": "…"}`. **Absolute paths work.** |
| `POST /template` (text/plain) | evaluates report-template text and **returns the result directly** (202, text/plain). This is the main way to read scene state. |
| `POST /abort`, `/pause`, `/resume`, `/stop` | job control (`/stop` stops the server) |

**Reading state:** report templates (`docs/realityscan-ref/reports_*.md`) emit whatever text we
ask for, so `rs-scene` makes them emit JSON (or one record per line for bulk data). What works:
- booleans via `$If(x,true)$If(!x,false)`; paths via `$EscapeBackslashes(...)`; quotes via
  `$Replace("\"","\\\"",…)`; lists end in a `null` sentinel instead of Epic's `$Strip(1)`, which
  breaks on empty lists.
- `$Using(...)` restricts the available variables: `cameraCount`/`pointCount` need
  `SfmExportFunctionSet`, and `ModelExportFunctionSet` errors (0x80070057) when no model exists.
- Sparse cloud: `$ExportPoints` as `x y z r g b track` lines (99 k points = 6.8 MB in 0.4 s);
  cameras: `$ExportCameras` with `x y z`, `R00..R22` (world→camera, `t = −R·C`), `f`
  (normalised by the 36 mm film width, along the long image side), `px py`.
- With no component selected, `$ExportCameras`/`$ExportPoints` return one bogus zero entry and
  the counts read 1; only query them when the state reports a selected component.

**Events** (`-RsRemoteStartNotifyREST <url>` POSTs JSON):
- `SEQUENCE_EXECUTION_STATE_UPDATE` {sequenceId, state: Running/Finished/Aborted, hr, hrMsg}
- `PEI_MAIN_EVENT_SECTION_STARTED` / `_COMPLETED` {algorithmId, level, dTime, hr, errorMessage}.
  IDs are in `docs/realityscan-ref/processids.md`.
- `SEQUENCE_EXECUTION_PROGRESS_UPDATE` {progress}
- `ON_TAG` {tag}, from `-tag NAME` markers we insert in sequences
- `HEALTH_CHECK_TEST_NOTIFY_REST` when notifications are enabled

### 3.4 Runtime options (both implemented 2026-10-14)

1. **Heroic install:** `~/Games/Heroic/RealityScan_2.2` + prefix
   `~/Games/Heroic/Prefixes/RealityScan 2.2`, launched with `umu-run` and the same env Heroic
   uses (`PROTONPATH`, `WINEPREFIX`, `STEAM_COMPAT_*`, `GAMEID=umu-0`,
   `PROTON_ENABLE_WAYLAND=0`, `PROTON_NVIDIA_NVCUDA=1`). Headless works without the Epic
   `-AUTH_*` arguments.
2. **Epic's Linux package** ([docs](https://dev.epicgames.com/documentation/en-us/realityscan/realityscan-for-linux),
   `RealityScan-2.2.deb`, 637 MB): **not a native build.** It is CrossOver 25
   (`ProductVersion 25.0.0.38555`) wrapping the identical `RealityScan.exe` and Remote Command
   plugin in a managed bottle (`/opt/realityscan/support/realityscan`, template `win10_64`).
   On first run CrossOver makes the user's private copy at `~/.realityscan/realityscan`
   (`$CX_BOTTLE_PATH` overrides the parent; mostly symlinks into the package) and refreshes it
   against the package on every launch, so a bottle created from one package location follows
   the package when it moves. Launch that works:
   `<root>/bin/wine --bottle=default --wait-children 'C:\Program Files\Epic Games\RealityScan\RealityScan.exe' <args>`.
   The package's own `realityscan-cli` (`--workdir … RealityScan.exe`, bare name) fails with
   `winewrapper.exe:error: cannot execute`. Unpacking the `.deb` with `bsdtar` into
   `~/.local/opt/realityscan` runs on CachyOS without root (the `wine` script finds its root
   from its own path). Inside a distrobox it also runs through
   `distrobox enter NAME -- sh script.sh` (`distrobox enter` joins its arguments and loses
   quoting), but the container must be created with `--nvidia`, otherwise nvcuda reports "Wine
   cannot find the cuda library" and CUDA is off; the user dropped the container route. The
   package ships **no JPEG XL decoder** (`-addFolder` of `.jxl` adds 0 images), so
   `Launcher::install_jxl_codec` copies `jxl_winthumb.dll` + `jxl_wicfix.dll` from the Heroic
   prefix into the bottle and registers them (`JXL_CODEC_REG`) after the first launch. REST
   comes up in 4–6 s (about 10 s through umu's pressure-vessel). Like the Windows install it
   lacks `tbbmalloc.dll`; its `nvcuda.dll` is an ELF Wine builtin that does find the host
   `libcuda.so.1`; alignment (CPU only there) and a CUDA preview model (139 k triangles from
   72 cameras in 4 s) verified, see §3.6.

`rs-backend::Install` carries a `Runtime` (`Proton{exe,plugin,prefix,proton_dir,umu_run}` |
`CrossOver{root,bottle,distrobox}`); `Install::detect_all()` lists Heroic, `/opt/realityscan`,
`~/.local/opt/realityscan` and running distroboxes; `REALITYSCAN_GTK_RUNTIME=heroic |
crossover[:ROOT] | distrobox:NAME[:ROOT]` overrides; the app's main menu has a radio submenu
("RealityScan Runtime") and keeps the choice in `settings.json` (`runtime`); switching
restarts RealityScan. Killing a distrobox instance can't follow the process tree (the Wine
processes aren't descendants of the `distrobox enter` client), so instances are also found by
`-setInstanceName <name>` on their command line (`proc_tree::kill_with_args`).

### 3.5 Alternatives considered

- **RSNode REST server** (`RSNode.exe`): works under Wine, but it binds 0.0.0.0 whatever
  `-hostAddress` says, runs each session in a sandbox folder (inputs resolve relative to
  `RSNodeData\sessions\<id>\_data`, outputs land in `_output`), and needs uploads or symlink
  tricks. Notes: `/project/create` returns the session ID in a `Session:` response header.
  Not used.
- **CLI delegation** (`-delegateTo`): one Wine process per command, blocked by umu's verb.
  Could work with `PROTON_VERB=runinprefix` (untested). Not needed.
- **gRPC** (same plugin): equivalent features; REST is simpler to debug. Could switch later.

---

### 3.6 Performance notes (2026-10-14; Ryzen AI 9 HX 370 laptop, 24 threads, RTX 5090 Laptop, "balanced" power profile)

Measured with `-newScene`, `-addFolder` (72 × 45 MP JPEG XL), `-align`, timing each command
through `/status` and reading the process's CPU time from `/proc`:

| phase | Heroic / Proton-CachyOS | Epic package / CrossOver 25 (host unpack) |
|---|---|---|
| import 72 JXL | 57 s | 64–71 s, ~10.5 cores busy |
| feature detection | cached (0 s) | 19 s cold, ~20 cores |
| reconstruction | 47–54 s, GPU peak 71 % | 41 s, GPU 0 % (CPU only), ~20 cores |
| REST server up | ~10 s | 4–6 s |

- Import time is the JPEG XL decode inside RealityScan (WIC → jxl-winthumb/libjxl, about
  10 CPU-seconds per image, both runtimes); plain JPEGs import in seconds. RealityScan caches
  features per image, so a second alignment of the same photos skips detection.
- All-core clock under load sat at a median 2.3 GHz (`scaling_cur_freq`); the firmware
  `platform_profile` was `balanced`. `performance` is the first thing to try for faster
  alignment on this laptop. (`scaling_max_freq` reads 2.0 GHz on every core while cores
  observably reach 4.7 GHz, so that value isn't the limiter.)
- The app's own thumbnails: 72 in 29 s (release build) after moving the decoder off a rayon
  pool — inside a 4-thread rayon pool, jxl-oxide's fork-join work stayed confined to those
  threads and each decode took 8 s instead of 0.8 s.
- Benchmark artefact worth remembering: `-detectFeatures` followed by `-align` as soon as
  `/status` looked idle aborted the alignment ("Operation aborted") twice; `-align` alone
  detects and reconstructs fine.

### 3.7 AI masking under Wine (2026-10-14)

`generateAIMasks` runs an ONNX model (`RealityScan.OnnxModelsPlugin.dll`, `onnxruntime.dll`
1.20.2 with the DirectML provider, `DirectML.dll` shipped). Measured on one 45 MP image: 50–58 s
wall, ~9 cores busy, GPU idle and no GPU memory allocated — the model runs on the CPU. The
`+dxcore` trace shows why: ONNX Runtime enumerates adapters through DXCore
(`DXCoreCreateAdapterFactory` → `CreateAdapterList` → `GetProperty(IsHardware)` →
`IsAttributeSupported(DXCORE_ADAPTER_ATTRIBUTE_D3D12_GRAPHICS)`), and Wine's dxcore (11.19,
Proton-CachyOS 11, CrossOver 25 alike) stubs `IsAttributeSupported` → FALSE, so the GPU is
rejected and no D3D12 device is ever created. `tools/dxcore/` holds a replacement built with
winegcc (DXGI-backed adapters, D3D12 attributes reported, `QueryInterface(IDXGIAdapter)`
forwarded for vkd3d-proton's `D3D12CreateDevice`); install = copy over
`system32/dxcore.dll` (a symlink to Proton's builtin in this prefix: remove it first) +
`DllOverrides\dxcore=native`. Setting the override while the symlink still pointed at the
builtin crashed RealityScan during masking. **Result:** one image 17.3 s wall (5.4 s of
processing, the rest model start-up), five images 26.5 s (2.8 s each after the first), 1.4
cores busy, 8 GB of GPU memory in use — down from 50–58 s per image on 9 cores.
`Launcher::install_dxcore` installs it automatically for Proton prefixes when the DLL is found
(`find_dxcore_dll`: `REALITYSCAN_GTK_DXCORE`, `share/realityscan-gtk/dxcore.dll`, the source
tree); `make dxcore` builds it, `make install` ships it when built. Untested on CrossOver
(vkd3d rather than vkd3d-proton), so not applied there. RealityScan keeps generated masks in
its opaque cache (`Temp/RealityScan/*.crdb`), so displaying them still depends on the export
format.

Mask export (`exportMasks folder params.xml`) is still unsolved: every root tried
(MaskLayer — the writer is `RealityScan.Export.MaskLayer` —, MaskExport, ExportMasks,
MaskImagesExport, MaskLayerExport, ImageExport, DepthNormalMask, the package's `masklayer.xml`
itself, with Header magics 0/MLX/MEX/EMX/LMX/MSX/MXX/ELX/IMX and versions 1/6, with and without
attributes) fails with 2147943500 "The physical end of the tape has been reached" + "Loading
of the configuration from the file … [0x4999\0x4\0x5555\0x4999\0x4]", also when every image
has a mask. The settings names RealityScan uses internally are `eiExportMasks`,
`eiExportImageList`, `eiExportImageListFileName`, `eiExportFileNaming`,
`eiExportMasksOrDepths`, `exportMaskLayer`. A params file saved from the GUI's "Export Mask
Images" dialog would settle it.

### 3.8 Headless alignment quirks (2026-11-02)

- A project aligned headless has **no reconstruction region**: RealityScan's GUI fits one when a
  component appears, the engine alone doesn't, and `exportReconstructionRegion` then fails with
  "Operation failed [0x4999\\0x4]" (status errorMessage "Translation failed."). As a failed
  command aborts the rest of its sequence, the app's combined region+mesh display export lost the
  mesh too: a 7.7 M-triangle model sat in RealityScan with nothing on screen. Fix: mesh commands
  first, region last, and when the region export fails for a component the first time the app
  sends `setReconstructionRegionAuto` (what the GUI would have done), which bumps the change
  count and re-runs the export.
- The ortho template fails with "There is no orthographic projection" on every refresh of a
  project without ortho projections (each a failed process with an error event); the app now
  asks once and stops until a project is opened or an ortho projection is calculated.
- Image files that share a name in one folder (`image_045.jpg` + `image_045.jxl`) are layers of
  one image to RealityScan ("Warning: trying to assign file … to already assigned image layer");
  it imports one photo per name. Two folders with the same file names are fine (144 images from
  2268a + 2268b). The app now warns about in-folder collisions when a folder is added.
- `-silent` auto-answers dialogs; the log shows them as `&Ok: 1` / `&Cancel: 3` lines.

### 3.9 Lasso selection and the orbit camera (2026-11-03)

- RealityScan's CLI has no way to pass a triangle set (only its own selectors), but
  `importModel file.ply` adds a model to the selected component in 0.1 s, names it "Model N"
  and selects it; binary PLY with `uchar` or `float` colours both import. So a lasso edit =
  select in the app's viewport → `Mesh::subset` → `write_ply` → `importModel` → the display
  export shows the new model. Textures don't survive (vertex colours do).
- Selection is resolved in the render callback against the depth buffer (`read_depth_buffer`),
  so only the visible surface inside the outline is taken: a triangle whose centroid is more
  than 0.3 % behind the drawn depth is occluded. Highlight = second index buffer drawn with a
  tint and polygon offset. In the sparse view the lasso picks cameras → image selection.
- `exportReconstructionRegion` fails with "Operation failed" while an **imported or preview
  model is selected** (`-selectModel "Model 5"` → fail; a normal model → fine; right after
  `setReconstructionRegionAuto` → fine). Lasso results are imported models, so after an edit
  the region can't be re-read: the app keeps the region it last read for the component
  (`region_known`) and only fits one automatically when none was ever read.
- RealityScan may write the region centre as `<CentreEuclid centre="x y z"/>` (after an
  automatic fit) rather than a `<centre>` child; the parser takes both.
- `gtk::GLArea::queue_render()` from inside the render handler is dropped; repaints the lasso
  and the deferred pivot orbit need are requested from an idle callback.
- The orbit camera had a bug: `eye()` scaled the elevation by cos(yaw), so pitch faded and the
  distance shrank as the view turned sideways (the "rotation feels off" report). Fixed, and
  orbiting now pivots around the point under the pointer (depth sampled on the next render;
  deltas that arrive before that are applied once the pivot is known), 0.0075 rad/px.

### 3.10 Abort, "Translation failed", the Run button (2026-11-03)

- `POST /abort` (also `/pause`, `/resume`) returns HTTP 400 for a bodiless request; with
  `Content-Type: application/json` and `{}` it returns 200. The yaml documents no body.
- "Translation failed. See into the console output for more information." is RealityScan's
  error for a failed **template** evaluation (process id 41061, listed as
  EXPORT_REGISTRATION_FILE). The app saw it while `calculateNormalModel` ran: the model-info
  template (`$ExportTextureInfo`) fails for a model that exists but isn't finished. Model info is
  now only queried while RealityScan is idle.
- Run = one sequence `align, tag, [setReconstructionRegionAuto when no component yet],
  calculateNormalModel, tag, calculateTexture, tag, tag RUN_DONE`; the usual tags drive the
  display exports once RealityScan is idle again.
- Speed relative to RealityScan's UI: the engine work is identical; what the app adds is
  thumbnail decoding (now at nice 10 so RealityScan's import keeps the cores) and the display
  exports after each stage (a textured 7.7 M-triangle model is a ~200 MB GLB plus an 8K PNG,
  seconds to load in release, far longer unoptimised). `[profile.dev] opt-level = 1` keeps dev
  runs usable; the release build is the one to judge by.

### 3.11 Second feedback round (2026-11-03)

- Run button moved beside the title (`header.pack_end`); in the pipeline box it overflowed into
  Align. Simplify dialog takes a triangle count or a percentage of the current count.
- Switching models: `Viewport::set_mesh` keeps the camera when the new mesh's bounds overlap
  the old ones (same object, different detail); and `sync_exports` loads an existing display
  file (`meshes/<guid>.ply|glb`) instead of re-exporting, with the files removed on
  `COLOR_DONE`/`TEXTURE_DONE` (the model changed) — so going back to a model is a load, not a
  RealityScan export.
- Model rows are `adw::ExpanderRow`s with `SwitchRow`s "Vertex colours" / "Texture" driving
  `Renderer::show_colors` (new `use_color` uniform) and `show_textures`.
- Control points: `CameraPose::project` (camera space x right, y down, z forward, matching
  `frustum_lines`; `f_px = focal × long side`, principal offset × long side) gives projected
  markers (dashed) in images where a triangulated point isn't measured; a click on one accepts
  it. Point rows expand to their measurements (image, pixel position; activating opens the
  image). Ctrl+click on the mesh unprojects the depth buffer, takes the nearest vertex normal,
  and measures the active point in up to three cameras facing that spot
  (`Cmd::AddMeasurements` → one CSV import).

### 3.12 Third feedback round (2026-11-03)

- A model shown with vertex colours stayed that way after texturing, and model rows had no
  details at all. Root cause: `/status` reports `isMasterBusy` for a moment after every
  template evaluation (a template is a process), and the idle check for the model-info query
  ran right after the state template, so it never passed. The idle flag is now read at the
  start of a refresh, before any template. Also: model info (which carries the textured flag)
  is only read while RealityScan is idle, and the display export was decided before it arrived. The backend now remembers whether the shown mesh had its texture
  (`mesh_textured`) and exports again when the flag turns up; model-info failures are reported
  in the log instead of silently skipped. Textured models up to 40 M triangles are shown in
  full (`REALITYSCAN_GTK_MAX_TEXTURED_PREVIEW`), because the simplified copy is untextured.
- Control point rows: measured images, then dimmed rows for the images the point projects
  into; an eye toggle hides the point's cross in the 3D view (`hidden_points`).
- Clicking near a dashed (projected) marker no longer snaps the measurement onto it: that
  moved clicks silently and read as points "bouncing around".

### 3.13 Ctrl+click points landing wrong (2026-11-03) — three causes

- **Same-named files in two folders.** The user's project holds `2268a\image_NNN.jxl` and
  `2268b\image_NNN.jxl` (144 inputs). The app matched cameras to images by file *stem*
  (template `imageName`) everywhere — camera double-click, image↔camera selection, projected
  markers, the per-point image lists and the 3D placement — so half the cameras mapped to the
  wrong folder's photo, and the measurement CSV named images by bare file name, which
  RealityScan resolves to the *first* input with that name. A Ctrl+click near the top was
  measured in folder A's images at folder B's pixel positions: a point mid-height. Fixes:
  the cameras template exports `originalImageIndex` (verified equal to the state template's
  `inputIndex`; the state's `inputCameraIndex` does NOT match `ExportCameras`' `index`, so it
  is not used) → `CameraPose::image_index`, `Scene::image_for_camera`,
  `sparse::camera_for_image` (name only as a fallback when unique); the CSV names images by
  their full stored path (`Z:\…\2268b\image_022.jxl` imports to the right input; a project
  relative path, forward slashes and a bare stem do not resolve/resolve to the last match).
- **Lens distortion.** `CameraPose::project` was a pinhole; the registered calibration is Brown
  (`k1..k4`, `t1`, `t2` from the cameras report; `fd`/`k1d`… are identical copies) applied on
  unit-focal-plane coordinates. Against RealityScan's own triangulations the Brown projection
  reproduces its reported max error exactly (0.83/0.83, 1.40/1.40, 1.71/1.71 px) while the
  pinhole is off 3–5 px near the centre and 33–86 px at r≈0.23–0.32 (these 45 MP crops have a
  principal point ~1200 px off centre, hence the earlier "validation" only held near the axis).
- **Narrow baselines.** The three *most frontal* cameras are adjacent frames 4–9° apart, so a
  few pixels of error moved the triangulation 0.1–0.7 units along the ray.
  `choose_placement_cameras` now takes the most frontal view, then the views adding the widest
  angle (greedy max-min angle).
- Diagnostics kept: every cached display file is dropped after Align/RunAll/DraftAlign/
  UpdateAlignment/MergeComponents/Open (frames can move under the same GUID), and the log
  reports the pick→triangulation distance after each 3D placement. Script step `log:<path>`
  dumps the log pane for scripted checks.

### 3.14 Release preparation (2026-11-05)

- Export dialog: the format is chosen in the options dialog (`adw::ComboRow`), the chooser gets
  the matching initial name and default filter, and the chosen extension is applied unless the
  user typed another model format's extension (GTK's `FileDialog` neither renames the file when
  its filter changes nor reports the chosen filter).
- Display export waits for the model's info: deciding PLY vs textured GLB with `info == None`
  showed a textured model plain and then exported it twice (`sync_exports` defers while the
  info query hasn't failed for this change count). A newly shown model resets the texture and
  vertex-colour switches to on.
- Added `LICENSE` (MIT), `.github/workflows/ci.yml` (Fedora container, rustup stable, fmt +
  clippy `-D warnings` + tests + release build), workspace `license`/`authors`/`repository`,
  AppStream URLs; README rewritten as a public setup guide (Heroic and Epic package routes,
  install, usage, troubleshooting, development, legal). `rust-version` is 1.88 (let chains);
  the newer MSRV enabled extra clippy lints (`as_chunks`, `is_multiple_of`, collapsible ifs),
  all fixed.

- Screenshots (2026-11-05, JPEG quality 88 from the PNG captures) are taken by driving a test copy attached to a probe instance with
  `REALITYSCAN_GTK_SCRIPT=@file` under `GDK_BACKEND=x11 GDK_SCALE=1`, captured with ImageMagick
  `import -window`; new steps `size:W:H`, `toggle:NAME[:off]`, `view:3d|image` make that
  repeatable. The image view's markers used to be joined by stray lines (cairo kept the text's
  current point; `new_path` per marker fixes it).

## 4. Open questions

1. **Licensing/auth headless:** does anything (saving, exporting, large projects) require the
   Epic login that Heroic passes (`-AUTH_*`, `-epicapp=…`)? Test with a real dataset.
2. **Template coverage:** confirm templates can expose everything the UI needs: camera poses
   (`$ExportCameras`), sparse points, control-point measurements, models/textures, ortho, and
   settings (`ConfigExportFunctionSet`/`GetProperty`). Where they can't, use exports.
3. **Large data transfer:** sparse clouds and meshes go through files (`exportSparsePointCloud`,
   `exportModel`/`exportLod`) into an app cache directory, never through templates.
4. ~~**GPU:** confirm CUDA works headless under Proton.~~ Resolved: see §3.2 (`PROTON_NVIDIA_NVCUDA`).
5. **Settings XMLs:** `exportModel`/`simplify`/etc. take parameter XMLs normally made in the UI.
   We must generate them ourselves, so collect samples (Windows UI or the shipped templates).
6. **Concurrency:** max 4 instances. One instance per open project, or one shared instance
   with `-load` switching?

---

## 5. Phases

Each phase ends with something usable. Phases 0–3 make it a practical daily driver; 4–7
work towards parity.

### Phase 0 — Foundations (done 2026-10-12)
- [x] Backend spike: Remote Command plugin REST, templates, events (§3).
- [x] Cargo workspace (`cargo fmt`/`clippy`/`test` clean; no CI yet, no remote).
- [x] `rs-client`: REST client, command builder, Wine path translation, event receiver,
      plus the `rsctl` example for manual testing.
- [x] `rs-backend`: Heroic install detection, launch with the `.rscmd` recipe, health wait,
      `-quit` shutdown, autosave listing/clearing. Verified live (instance up in ~9 s).
- [x] `rs-scene`: `templates/state.tpl` emits valid JSON (project, images with flags,
      components, models) → `Scene`.
- [x] App skeleton (`crates/realityscan-gtk`): backend thread + channels, main window with
      status/subtitle, image list, add images/folder, align, abort, refresh, progress, log;
      closing the window quits RealityScan first. Verified live.
- Not done: preferences UI for backend paths (auto-detect only), autosave recovery UI.
- Added later the same day: GIO actions with shortcuts (Ctrl+O/S/Shift+S/I/M/L/R/Q, F5, Esc),
  primary menu + About, status bar with live activity and inline progress, collapsible log,
  "RealityScan is not running" banner with restart (Ctrl+R), `realityscan-gtk <project.rsproj>`
  opens a project at start, `REALITYSCAN_GTK_ATTACH=<url>` attaches to a running instance.

### Phase 1 — Projects and images
- [x] Open/save `.rsproj`, add images/folder (basic). Recent projects (menu, `recent.json`),
      drag-and-drop of images/folders/projects onto the window. Autosave handling: a banner
      reports autosaves left by a crashed instance and can discard them (registry entries
      cleared with `reg.exe` run through umu/Proton). **Recovery itself is not possible
      headless**: `-load` of the `.rsproj.autosave` crashes RealityScan (it is a small metadata
      XML pointing at a `New.autosave/` folder, meant for RealityScan's own recovery prompt), so
      the banner points the user at RealityScan's window via Heroic.
- Import video (`importVideo`), HDR, laser scans.
- [x] Image grid with thumbnails (JXL via `jxl-oxide`, others via `image`; cached as JPEG in
      `~/.cache/realityscan-gtk/thumbs`). Dev builds need `[profile.dev.package."*"]
      opt-level = 3` or decoding is unusable. TODO: reduced-resolution JXL decode for speed.
- [x] Per-image enable switches for alignment / meshing / texturing (grid selection + the
      sidebar's images menu; disabled stages show as "mesh off" etc. on the cell). Weights,
      layers, calibration and lens groups, EXIF: TODO.
- [x] Image selection ↔ camera selection: selecting thumbnails highlights their frusta, clicking a
      camera in the viewport selects its image. Selection is not mirrored into RealityScan
      (`-selectImage` is only used transiently for the switches).
- Task panel: queue, progress (events), cancel/pause/resume, log.

### Phase 2 — Alignment and components
- [x] Align (basic), **3D viewport v1** (`rs-render` on `GtkGLArea`): sparse cloud with colours,
      camera frusta, world axes, orbit/pan/zoom, outlier-robust framing. Verified on a 72-image
      set. GL entry points come from libepoxy's `epoxy_*` dispatch variables (the `epoxy` crate is
      unbuildable: yanked `xml-rs`).
- [x] Alignment settings (the Align button's arrow): feature detection quality, features per
      mpx/image, overlap, downscale, reprojection error, preselector, detector sensitivity,
      distortion model, auto region, force rematch, applied with `-set`. Values can't be read
      back (`$GetProperty` only sees report variables), so the last applied ones are kept in
      `~/.config/realityscan-gtk/settings.json`. Draft / update: TODO.
- [x] Component list (select, rename, delete). Merge, export, import: TODO.
- [x] Viewport: camera picking, selection highlight, reconstruction-region box.
- Registration export and import (XMP, Bundler, COLMAP), undistorted images.

### Phase 3 — Reconstruction, texturing, export (core pipeline)
- [x] First end-to-end run (2026-10-12): auto region → normal-detail model on 72 × 45 MP images
      (~12 min, CUDA depth maps + CPU meshing) → 5 M triangles → `exportSelectedModel` to PLY
      (125 MB in 3.6 s) → parsed and rendered lit in the viewport. Notes: RealityScan's PLY
      stores colours as `float` 0..1; saved projects store image folders relative to the
      `.rsproj`; meshes above 15 M triangles are not previewed (simplify first, TODO).
- [x] Reconstruction region: auto / by density, numeric centre/size/yaw-pitch-roll panel
      (writes an `.rsbox` → `setReconstructionRegion`), box drawn in the viewport with an
      **interactive gizmo** (drag a face to move it, the centre to move the box; the result is
      sent on release). Rotation only through the panel; "on control points": TODO.
- Ground plane and gravity tools.
- [x] Preview/normal/high model; model list (select, duplicate, rename, delete, export) with
      triangle count and colour/texture badges.
- [x] Model detail choice (preview/normal/high) and vertex colours ("Color", which re-exports the
      mesh so it shows coloured in the viewport); texture (basic). Texturing a 5 M-triangle model
      from 72 × 45 MP images: unwrap 43 s, preprocessing ~6 min, then GPU bursts.
- [x] Texture display in the viewport (textured models are exported as GLB, the texture is
      uploaded as a mipmapped GL texture; "Texture" toggle switches to vertex colours).
      Quality texture, reproject, colour correction: TODO.
- [x] Simplify (to a triangle count), smooth, close holes, clean, keep largest part. Fill holes,
      the parameter XMLs for smooth/unwrap: TODO.
- [x] Big models: above 15 M triangles the app previews a temporary simplified copy
      (`simplify` → export → delete → reselect, one sequence; 3 M triangles by default,
      `REALITYSCAN_GTK_MAX_PREVIEW` / `REALITYSCAN_GTK_PREVIEW_TARGET` override). Display
      exports live in `~/.cache/realityscan-gtk/meshes` keyed by model GUID (a 5 M-triangle
      textured model is ~180 MB per export); files older than three days are pruned at start-up.
      True LOD streaming: TODO.
- [x] Export dialog (options → file chooser; obj/ply/glb/fbx/usd(z)/abc/dae/dxf/stl/3mf/xyz/las/ptx)
      with a generated settings XML; toast with "Show" opens the folder. `exportLod`,
      `export3dTiles`, zip, Sketchfab: TODO.

### Phase 4 — Control points and constraints
- [x] 2D image view (double-click a thumbnail or a camera in the 3D view): GPU-textured
      full-resolution image, wheel zoom around the pointer, drag pan, pixel readout;
      measurements drawn as labelled markers; a click places a measurement for the active
      control point, dragging a marker moves it (delete + re-import in one sequence), a
      right-click removes it. Per-measurement residual overlay: TODO (the per-point maximum
      error is shown in the list).
- [x] Control point list (name, measurement count, triangulated position, max reprojection
      error; rename, delete; "+" creates a point by naming it, its first click creates it in
      RealityScan), marker detection (`detectMarkers`), ground control import/export and
      measurement export (CSV). By-error selection: TODO.
- [x] Distance constraints: "Distances +" dialog (`defineDistance`), list with defined vs
      calculated length and error, delete; Tools › Update Alignment runs `-update`, which
      rigidly rescales components *and* models to fit (verified: 10.76 → 100.0003 units in
      0.6 s). Control points are drawn as crosses in the 3D view.
- [x] Components: merge, export (`.rsalign`), import.
- Coordinate systems (project/output), georeferencing, flight logs/trajectories: TODO.

### Phase 5 — Mesh selection and editing
- [x] RealityScan's selectors (inside/outside region, marginal, large triangles, largest
      component, invert, deselect) under Tools › Select Triangles, filter selection
      (`removeSelectedTriangles`, makes a new model) and cut by region (`cutByBox`). Fill holes.
      Lasso/rect/brush selection in our viewport is impossible through the CLI (there is no
      way to hand RealityScan a triangle set); the region box is the only spatial selector.
      Undercut: TODO.
- [x] Masks: AI masks (`generateAIMasks`, ~35 s per 45 MP image on this machine, runs as a
      background job that `/abort` does not stop; it honours the image selection, so the app
      offers it per image, per selection and for all), masks from the selected model
      (`generateMaskFromMesh`), import from a folder of images named like the photos
      (`setImageLayer <index> <file> mask`), export (`exportMasks <folder>`), and a brush in the
      image view: "Mask" shows the mask as a red overlay, left-drag excludes, right-drag or the
      erase tool restores, Apply writes a PNG (white = keep, black = excluded) and sets it as
      the image's mask layer; Clear removes the layer. Masks are cached per image stem in
      `~/.cache/realityscan-gtk/masks`. **Export of RealityScan's own masks (AI or from mesh)
      is not working yet**: `exportMasks` needs a settings XML whose root/magic I could not
      find (`MaskExport` loads but writes nothing; `MaskLayerExport`, `MasksExport`,
      `ExportMasks`, `MaskLayer`, `ImageLayerExport` fail), so AI masks are used by RealityScan
      but can't be shown in the image view. Exporting one from the Windows UI's "Export Mask
      Images" dialog once would reveal the format. Beware: a failing `exportMasks` with a bad
      settings file made RealityScan *exit* ("Save Project… the currently opened project will
      be closed", plugin unloaded), so the app never sends that command without a known format.
- [x] Alignment extras: draft alignment, feature detection, texture variants (unwrap only,
      quality texture, preview/quality vertex colours, colour correction), exports (sparse
      point cloud, registration, undistorted images, XMP camera files).

### Phase 6 — Ortho, cross-sections, classification, reports
- [x] Ortho projections: "Ortho projections +" renders the selected model through a face of
      the reconstruction region (`.rsortho` written by `rs_scene::ortho`; `boxSideConerIndex`
      0/8/16 look along X/Y/Z, the index within each four rotates the image; pixel size keeps
      the face's aspect), the list shows size, and each row can be viewed in the image view or
      exported (`exportOrthoProjection name file params.xml`; a params file is mandatory, two
      arguments are read as `file params.xml`; the working settings root is `OrthoExport` with
      header magic 0; RealityScan appends the image extension itself and also writes a `.tfw`
      world file). Shapes import/export: TODO.
- Cross-sections and contours.
- Classification (DTM classify, classes, formats, colour by class).
- Reports (export/print with templates; render HTML reports in a WebKitGTK view).
- Camera snapshots and render-from-position tools.

### Phase 7 — Polish and parity sweep
- Walk through every Windows ribbon tab and settings panel and tick off parity (checklist
  in `docs/parity.md`, generated from the CLI command list and the Help's `appbasics/*`
  pages).
- Undo where RealityScan supports it, keyboard shortcuts, layouts (multi-view splits like
  the original 1/2/3/4-pane layouts).
- [x] Packaging (native): `packaging/` has the desktop entry, SVG icon, AppStream metainfo
      and a PKGBUILD; `make install` (PREFIX=~/.local by default) installs the release build.
      Flatpak is deliberately not done: the app has to run umu/Proton from the host, which a
      sandbox can't do without `flatpak-spawn --host` tricks.
- Translations, accessibility, documentation.

---

## 6. Risks

| Risk | Mitigation |
|---|---|
| A dialog appears despite `-silent` and stalls the instance | Watch `/status` and events for stalls; detect RealityScan windows; offer "show RealityScan window" as an escape hatch |
| Templates can't expose some state | Exports into a cache dir; keep a UI-side shadow model of what we've told RealityScan |
| Huge meshes/clouds overwhelm the viewer | LOD export, streaming, GPU-friendly formats; never load full-res by default |
| Interactive tools have no CLI equivalent | Check each against the command list early; fall back to "open in RealityScan" for any that can't be done |
| Plugin/CLI changes between RealityScan versions | Version check via the `$(appVersion)` template; integration tests against a fixture dataset |
| `GDK_BACKEND=x11` quit crash (GTK IBus module, `gdk_x11_surface_get_xid`) | Dev-only; `GTK_IM_MODULE=simple`. Wayland is unaffected |
| Licensing/EULA | Use only documented interfaces (CLI, Remote Command plugin); don't redistribute RealityScan or its files; check the EULA before any public release |

---

## 7. References

- RealityScan install: `~/Games/Heroic/RealityScan_2.2` (Heroic/Epic), Wine prefix
  `~/Games/Heroic/Prefixes/RealityScan 2.2`, runtime GE-Proton 11-5 via umu.
- Local docs (in the install): `Help/en-US/tutorials/commandline*.htm` (CLI),
  `Help/en-US/appbasics/allcommands.htm` (all commands),
  `Help/en-US/tutorials/setkeyvaluetable.htm` (`-set` keys),
  `Help/en-US/appbasics/reports_*.htm` (template language; extracted to `docs/realityscan-ref/`).
- Remote Command plugin: OpenAPI and protos in `docs/realityscan-ref/` (from Epic's samples zip).
- Wine fixes already in that prefix: JXL WIC decoder wrapper (`jxl_wicfix.dll`) and popup
  stylesheet fix (`rsdlgfx.dll` + exe patch). Not needed by this app, but documented there.
