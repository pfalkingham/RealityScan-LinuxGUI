//! The RealityScan backend, run on its own thread with a tokio runtime.
//!
//! The UI sends [`Cmd`]s and receives [`Msg`]s over channels; nothing here
//! touches GTK.
//!
//! Normally the backend launches its own headless RealityScan. For development,
//! `REALITYSCAN_GTK_ATTACH=http://127.0.0.1:<port>` attaches to an instance that
//! is already running (and leaves it running on quit).
//!
//! One rule governs everything that sends commands: **RealityScan aborts a running
//! process when a command arrives**, so commands are only sent while it is idle,
//! and the backend's own exports (mesh for display, reconstruction region) go out
//! as a single sequence so they never race each other.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use rs_backend::{Install, Instance, LaunchOptions, Launcher};
use rs_client::{Client, Command, CommandSeq, Event, EventReceiver, SequenceState, Status, path::to_wine};
use rs_scene::Scene;
use rs_scene::controlpoints::ControlPointData;
use rs_scene::export::ModelExportOptions;
use rs_scene::mesh::Mesh;
use rs_scene::ortho::{OrthoProjection, OrthoRequest};
use rs_scene::region::Region;
use rs_scene::sparse::{CameraPose, SparseCloud};

/// Meshes above this are previewed through a temporary simplified copy.
fn max_preview_triangles() -> u64 {
    std::env::var("REALITYSCAN_GTK_MAX_PREVIEW").ok().and_then(|v| v.parse().ok()).unwrap_or(15_000_000)
}

/// Textured models are shown in full up to this size: the simplified copy made for bigger
/// meshes loses the texture (`simplify` makes an untextured model), so the limit is higher.
fn max_textured_preview_triangles() -> u64 {
    std::env::var("REALITYSCAN_GTK_MAX_TEXTURED_PREVIEW")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(40_000_000)
}

/// Triangle count of the temporary preview copy made for oversized meshes.
fn preview_target_triangles() -> u64 {
    std::env::var("REALITYSCAN_GTK_PREVIEW_TARGET").ok().and_then(|v| v.parse().ok()).unwrap_or(3_000_000)
}

/// Reconstruction detail levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Detail {
    Preview,
    Normal,
    High,
}

/// Triangle selectors offered by RealityScan's CLI.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Selector {
    InsideRegion,
    OutsideRegion,
    Marginal,
    /// Edges longer than this multiple of the average edge.
    LargeRel(f64),
    LargestPart,
    Invert,
    None,
}

impl Selector {
    fn command(self) -> Command {
        match self {
            Self::InsideRegion => Command::new("selectTrianglesInsideReconReg"),
            Self::OutsideRegion => Command::new("selectTrianglesOutsideReconReg"),
            Self::Marginal => Command::new("selectMarginalTriangles"),
            Self::LargeRel(t) => Command::new("selectLargeTrianglesRel").arg(t.to_string()),
            Self::LargestPart => Command::new("selectLargestModelComponent"),
            Self::Invert => Command::new("invertTrianglesSelection"),
            Self::None => Command::new("deselectModelTriangles"),
        }
    }
}

/// Texturing and colouring variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextureOp {
    Unwrap,
    QualityTexture,
    CorrectColors,
    PreviewColors,
    QualityColors,
}

/// Per-image pipeline switches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFlag {
    Alignment,
    Meshing,
    Texturing,
}

impl ImageFlag {
    fn command(self) -> &'static str {
        match self {
            Self::Alignment => "enableAlignment",
            Self::Meshing => "enableMeshing",
            Self::Texturing => "enableTexturingAndColoring",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Alignment => "alignment",
            Self::Meshing => "meshing",
            Self::Texturing => "texturing and colouring",
        }
    }
}

/// A mesh handed to [`Cmd::ImportMesh`]; debug-prints as a size rather than its vertices.
#[derive(Clone)]
pub struct EditMesh(pub Arc<Mesh>);

impl std::fmt::Debug for EditMesh {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "EditMesh({} triangles)", self.0.triangle_count())
    }
}

/// Requests from the UI.
#[derive(Debug)]
pub enum Cmd {
    AddImages(Vec<PathBuf>),
    AddFolder(PathBuf),
    /// `-load` an `.rsproj`.
    Open(PathBuf),
    /// `-save`: in place when `None`, otherwise to the given `.rsproj`.
    Save(Option<PathBuf>),
    /// Enable or disable images (RealityScan paths, as the scene lists them) for a stage.
    SetImageFlag {
        images: Vec<String>,
        flag: ImageFlag,
        enabled: bool,
    },
    Align,
    /// Component operations address the component by name (RealityScan's commands act on
    /// the selected one, so each is sent as "select, then act" in one sequence).
    SelectComponent(String),
    RenameComponent {
        component: String,
        new_name: String,
    },
    DeleteComponent(String),
    /// Auto reconstruction region + model at the given detail.
    Model(Detail),
    SelectModel(String),
    RenameModel {
        model: String,
        new_name: String,
    },
    DeleteModel(String),
    DuplicateModel(String),
    /// Bake per-vertex colours into the selected model (shows up in the viewport).
    Colorize,
    /// Texture the selected model.
    Texture,
    /// Simplify the selected model to about this many triangles.
    Simplify(u64),
    Smooth,
    CloseHoles,
    CleanModel,
    /// Remove every part of the model except the largest connected one.
    KeepLargestPart,
    /// Export a model (by name) to a file; the format follows the extension.
    ExportModel {
        model: String,
        path: PathBuf,
        options: ModelExportOptions,
    },
    /// Replace the reconstruction region.
    SetRegion(Region),
    RegionAuto,
    RegionByDensity,
    /// Export the selected model and show it in the viewport.
    ShowMesh,
    Abort,
    /// Re-read the scene state.
    Refresh,
    /// Apply `-set key=value` settings (alignment, reconstruction, …).
    SetSettings(Vec<(String, String)>),
    /// Place a control-point measurement: `image` is the image file name (with extension),
    /// `x`/`y` pixel coordinates. A new `point` name creates the control point.
    AddMeasurement {
        point: String,
        image: String,
        x: f64,
        y: f64,
    },
    RenameControlPoint {
        point: String,
        new_name: String,
    },
    /// Delete a control point by index.
    DeleteControlPoint(u32),
    /// Move a measurement: delete it and import it again at the new position.
    MoveMeasurement {
        point: String,
        index: u32,
        image: String,
        x: f64,
        y: f64,
    },
    ImportGroundControl(PathBuf),
    ExportGroundControl(PathBuf),
    ExportMeasurements(PathBuf),
    /// Select model triangles with one of RealityScan's selectors.
    SelectTriangles(Selector),
    /// Make a new model without the selected triangles.
    RemoveSelectedTriangles,
    /// Make a new model keeping only the triangles inside (or outside) the region.
    CutByRegion {
        keep_inside: bool,
        fill_holes: bool,
    },
    FillHoles,
    GenerateMasks,
    /// AI masks for these images only (RealityScan paths as the scene lists them).
    GenerateMasksFor(Vec<String>),
    DraftAlign,
    DetectFeatures,
    TextureOp(TextureOp),
    ExportSparseCloud(PathBuf),
    ExportRegistration(PathBuf),
    ExportUndistorted(PathBuf),
    ExportXmp,
    /// Set a mask image (white = keep, black = excluded) on the image with this 1Ds index.
    ApplyMask {
        image_index: u32,
        path: PathBuf,
    },
    /// Remove the mask layer from an image (RealityScan path).
    RemoveMask(String),
    /// Set masks from a folder of `<image stem>.png|jpg|tif` files.
    ImportMasks(PathBuf),
    ExportMasks(PathBuf),
    MaskFromMesh,
    CalculateOrtho(OrthoRequest),
    ExportOrtho {
        name: String,
        path: PathBuf,
    },
    /// Delete one measurement (`index` within the point) of a control point.
    DeleteMeasurement {
        point: String,
        index: u32,
    },
    DefineDistance {
        a: String,
        b: String,
        distance: f64,
        name: Option<String>,
    },
    DeleteConstraint(u32),
    DetectMarkers,
    /// `-update`: rigidly fit components and models to the control points and constraints.
    UpdateAlignment,
    MergeComponents,
    ExportComponent {
        component: String,
        path: PathBuf,
    },
    ImportComponent(PathBuf),
    /// Forget RealityScan's pending autosaves (registry entries) without recovering them.
    DiscardAutosaves,
    /// Start (or re-attach to) RealityScan again after it was lost.
    Relaunch,
    /// Save `spec` as the preferred RealityScan runtime (see [`Install::from_spec`]) and
    /// restart RealityScan with it.
    SetRuntime(String),
    /// Several measurements in one import: (image file name, control point, x, y).
    AddMeasurements(Vec<(String, String, f64, f64)>),
    /// The whole workflow in one go, like RealityScan's own Start button: align, a normal-detail
    /// model and a texture, one after another (with an automatic region when the project has
    /// no component yet).
    RunAll,
    /// Make a new model from the displayed mesh with only the triangles `keep` marks (a lasso
    /// edit): written as PLY and `importModel`ed, since RealityScan's own triangle selection
    /// can't be fed from outside.
    ImportMesh {
        mesh: EditMesh,
        keep: Vec<bool>,
    },
    /// Shut RealityScan down; the backend replies with [`Msg::Stopped`] and exits.
    Quit,
}

/// Notifications to the UI.
#[derive(Debug)]
pub enum Msg {
    Starting(String),
    Ready {
        version: String,
        /// False when attached to an instance we didn't start.
        owned: bool,
        /// "Proton" or "CrossOver"; empty when attached.
        runtime: String,
    },
    Failed(String),
    /// The RealityScan runtimes found on this machine, as (spec, label), and the one in use.
    Runtimes {
        available: Vec<(String, String)>,
        current: Option<String>,
    },
    Status(Status),
    Scene(Scene),
    /// Sparse cloud and cameras of the selected component (empty when none).
    Sparse {
        cloud: SparseCloud,
        cameras: Vec<CameraPose>,
    },
    /// A mesh to display, or `None` to show the sparse cloud again.
    Mesh(Option<Arc<Mesh>>),
    /// The reconstruction region of the selected component (`None` when there is none).
    Region(Option<Region>),
    /// Control points, measurements and distance constraints of the project.
    ControlPoints(ControlPointData),
    Orthos(Vec<OrthoProjection>),
    /// A user export finished and the file exists.
    Exported(PathBuf),
    /// RealityScan left autosaves behind (a previous instance didn't exit cleanly).
    Autosaves(Vec<AutosaveInfo>),
    Event(Event),
    /// Latest line of RealityScan's own console log, e.g. which image it's on.
    Activity(String),
    Log(String),
    /// RealityScan is gone; the UI should offer [`Cmd::Relaunch`].
    Disconnected,
    Stopped,
}

/// A recoverable autosave, as shown to the user.
#[derive(Debug, Clone)]
pub struct AutosaveInfo {
    /// The `.rsproj.autosave` to `-load`, when it exists.
    pub project_file: Option<PathBuf>,
    pub modified: Option<std::time::SystemTime>,
}

pub struct Handle {
    pub cmds: async_channel::Sender<Cmd>,
    pub msgs: async_channel::Receiver<Msg>,
}

impl Handle {
    pub fn send(&self, cmd: Cmd) {
        if let Err(e) = self.cmds.send_blocking(cmd) {
            tracing::warn!("backend gone: {e}");
        }
    }
}

/// Spawn the backend thread. RealityScan starts immediately.
pub fn start() -> Handle {
    let (cmd_tx, cmd_rx) = async_channel::unbounded::<Cmd>();
    let (msg_tx, msg_rx) = async_channel::unbounded::<Msg>();
    std::thread::Builder::new()
        .name("rs-backend".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("tokio runtime");
            rt.block_on(run(cmd_rx, msg_tx));
        })
        .expect("spawn backend thread");
    Handle { cmds: cmd_tx, msgs: msg_rx }
}

/// What we're talking to: an instance we own, or one we only attached to.
struct Session {
    client: Client,
    events: Option<EventReceiver>,
    instance: Option<Instance>,
    /// The install the instance was started from (`None` when attached).
    install: Option<Install>,
}

/// Identity of the sparse data last sent to the UI: selected component plus its sizes.
type SparseKey = (String, u32, u64);

#[derive(Default)]
struct State {
    scene: Option<Scene>,
    last_sparse: Option<SparseKey>,
    /// GUID of the model currently shown as a mesh (or deliberately skipped).
    mesh_for: Option<String>,
    /// Whether the shown mesh was exported with its texture. Model info can arrive after the
    /// display export was decided (it is only queried while RealityScan is idle), in which case
    /// a model first shown with vertex colours is exported again with the texture.
    mesh_textured: bool,
    /// Export in flight: (model GUID, mesh file), resolved when `MESH_EXPORTED` arrives.
    pending_mesh: Option<(String, PathBuf)>,
    /// Project change count the displayed region belongs to.
    region_for: Option<(String, u64)>,
    /// Region export in flight: (component GUID, change count, rsbox path).
    pending_region: Option<(String, u64, PathBuf)>,
    /// A user export in flight, reported when `EXPORT_DONE` arrives.
    pending_export: Option<PathBuf>,
    /// When the pending exports were sent; idle status long after that means they failed.
    pending_since: Option<std::time::Instant>,
    /// Commands received while RealityScan was busy, sent once it is idle.
    queued: Vec<Cmd>,
    /// Project change count at which the model-info template last failed (don't retry until
    /// the project changes: the failure itself produces a sequence event).
    model_info_failed_at: Option<u64>,
    /// The failure's message, reported to the user once by the next refresh.
    model_info_error: Option<String>,
    /// Some model still lacks its details (RealityScan was busy when they were asked for);
    /// refresh again on the next idle tick.
    info_pending: bool,
    /// When the state was last refreshed, to coalesce event-driven refreshes.
    last_refresh: Option<std::time::Instant>,
    /// A coalesced refresh is owed on the next tick.
    refresh_pending: bool,
    /// A display export was deferred because RealityScan was busy; retry when idle.
    sync_pending: bool,
    /// A command went out since the last refresh, so `scene` may be out of date.
    scene_stale: bool,
    /// Component whose region export failed and for which an automatic region was requested
    /// (a fresh headless alignment has none; RealityScan's GUI would have made one).
    region_auto_tried: Option<String>,
    /// `setReconstructionRegionAuto` owed on the next idle tick, for this component.
    region_auto_pending: Option<String>,
    /// Components whose region has been read at least once this session. Their later export
    /// failures (RealityScan can't export the region while an imported or preview model is
    /// selected) keep the region on screen instead of fitting a new one.
    region_known: std::collections::HashSet<String>,
    /// The ortho template failed because the project has no ortho projections; don't ask again
    /// until a project is opened or an ortho projection is calculated (each failed query is a
    /// failed process with an error event).
    orthos_unavailable: bool,
    /// When the last command went out. RealityScan reports a sequence as finished slightly
    /// before its effects show in the templates (a deleted model was still listed 0.4 s
    /// later), so state read within [`COMMAND_SETTLE`] of a command is not acted on.
    command_sent_at: Option<std::time::Instant>,
}

/// How long after a command the scene is considered unsettled.
const COMMAND_SETTLE: Duration = Duration::from_millis(1500);

impl State {
    fn settling(&self) -> bool {
        self.command_sent_at.is_some_and(|t| t.elapsed() < COMMAND_SETTLE)
    }
}

enum Outcome {
    /// The user asked to quit; RealityScan has been shut down (if we owned it).
    Quit,
    /// The user picked another runtime; RealityScan has been shut down and should start again.
    Relaunch,
    /// Contact with RealityScan was lost, or it never started.
    Lost,
}

pub fn cache_dir(sub: &str) -> PathBuf {
    dirs::cache_dir().unwrap_or_else(std::env::temp_dir).join("realityscan-gtk").join(sub)
}

/// Display exports are keyed by model GUID and never reused once a model is gone, so drop
/// cached meshes and regions older than this at start-up.
const CACHE_MAX_AGE: Duration = Duration::from_secs(3 * 24 * 3600);

fn prune_cache(send: &impl Fn(Msg)) {
    let mut freed = 0u64;
    for sub in ["meshes", "regions"] {
        let Ok(entries) = std::fs::read_dir(cache_dir(sub)) else { continue };
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else { continue };
            let old =
                meta.modified().ok().and_then(|m| m.elapsed().ok()).is_some_and(|age| age > CACHE_MAX_AGE);
            if meta.is_file() && old && std::fs::remove_file(entry.path()).is_ok() {
                freed += meta.len();
            }
        }
    }
    if freed > 0 {
        send(Msg::Log(format!("Removed {} MB of old display exports from the cache", freed / 1_000_000)));
    }
}

async fn run(cmds: async_channel::Receiver<Cmd>, msgs: async_channel::Sender<Msg>) {
    let send = |m: Msg| {
        if let Msg::Log(text) = &m {
            tracing::info!(target: "rs", "{text}");
        }
        let _ = msgs.send_blocking(m);
    };
    let attach_url = std::env::var("REALITYSCAN_GTK_ATTACH").ok();
    prune_cache(&send);
    loop {
        let session = match &attach_url {
            Some(url) => attach(url, &send).await,
            None => {
                let install = choose_install(&send).await;
                launch(install, &send).await
            }
        };
        let outcome = match session {
            Some(session) => session_loop(session, &cmds, &send).await,
            None => Outcome::Lost,
        };
        if matches!(outcome, Outcome::Quit) {
            break;
        }
        if matches!(outcome, Outcome::Relaunch) {
            continue;
        }
        // Wait for the user to ask for a restart (or to quit).
        send(Msg::Disconnected);
        loop {
            match cmds.recv().await {
                Ok(Cmd::Relaunch) => break,
                Ok(Cmd::SetRuntime(spec)) => {
                    crate::settings::set(crate::settings::RUNTIME_KEY, &spec);
                    break;
                }
                Ok(Cmd::Quit) | Err(_) => {
                    send(Msg::Stopped);
                    return;
                }
                Ok(Cmd::DiscardAutosaves) => discard_autosaves(&send).await,
                Ok(_) => send(Msg::Log("RealityScan isn't running; restart it first".into())),
            }
        }
    }
    send(Msg::Stopped);
}

/// Serve the UI until it quits or RealityScan disappears.
async fn session_loop(
    mut session: Session,
    cmds: &async_channel::Receiver<Cmd>,
    send: &impl Fn(Msg),
) -> Outcome {
    let rs_log = session
        .install
        .as_ref()
        .and_then(|i| i.realityscan_log())
        .or_else(|| Install::detect_heroic().ok().and_then(|i| i.realityscan_log()));
    let mut st = State::default();

    match rs_scene::fetch_state(&session.client).await {
        Ok(scene) => {
            send(Msg::Ready {
                version: scene.version.clone(),
                owned: session.instance.is_some(),
                runtime: session.install.as_ref().map(|i| i.short_label().to_owned()).unwrap_or_default(),
            });
            send(Msg::Scene(scene));
            refresh(&session.client, send, &mut st).await;
            sync_exports(&session.client, send, &mut st, false).await;
        }
        Err(e) => send(Msg::Log(format!("RealityScan answered but the state query failed: {e}"))),
    }

    let mut tick = tokio::time::interval(Duration::from_secs(1));
    let mut last_change: Option<u64> = None;
    let mut last_image_count: Option<usize> = None;
    let mut was_busy = false;
    // Seed with the current last line so old entries aren't shown as live activity.
    let mut last_activity = rs_log.as_deref().and_then(last_log_line).unwrap_or_default();
    let outcome = loop {
        tokio::select! {
            cmd = cmds.recv() => {
                let Ok(cmd) = cmd else { break Outcome::Quit };
                if matches!(cmd, Cmd::Quit) {
                    break Outcome::Quit;
                }
                if let Cmd::SetRuntime(spec) = &cmd {
                    crate::settings::set(crate::settings::RUNTIME_KEY, spec);
                    send(Msg::Log(format!("Runtime set to {spec}; restarting RealityScan")));
                    break Outcome::Relaunch;
                }
                handle_cmd(cmd, &session.client, send, &mut st).await;
            }
            event = recv_event(&mut session.events) => {
                if let Some(event) = event {
                    tracing::debug!(target: "rs", "event: {event:?}");
                    match &event {
                        // Template evaluations and other processes emit these too, so they say
                        // nothing about our own exports; those are judged by their tags.
                        Event::SequenceState { state: SequenceState::Finished | SequenceState::Aborted, .. } => {
                            refresh_debounced(&session.client, send, &mut st).await;
                            sync_exports(&session.client, send, &mut st, false).await;
                        }
                        Event::Tag(tag) => match tag.as_str() {
                            "MESH_EXPORTED" => load_pending_mesh(send, &mut st).await,
                            "REGION_EXPORTED" => load_pending_region(send, &mut st),
                            "EXPORT_DONE" => {
                                if let Some(path) = st.pending_export.take() {
                                    // Some exporters add their own extension (ortho images).
                                    let actual = rs_scene::ortho::ortho_output_candidates(&path)
                                        .into_iter()
                                        .find(|p| p.exists());
                                    match actual {
                                        Some(p) => send(Msg::Exported(p)),
                                        None => send(Msg::Log(format!("Export finished but {} is missing", path.display()))),
                                    }
                                }
                            }
                            // The mesh changed; make the next sync export it again.
                            "COLOR_DONE" | "TEXTURE_DONE" => {
                                // The displayed model changed on RealityScan's side: its
                                // display file is stale.
                                if let Some(guid) = st.mesh_for.take() {
                                    forget_display_files(&guid);
                                }
                            }
                            "MESH_EDITED" => st.mesh_for = None,
                            "RUN_DONE" => send(Msg::Log("Workflow finished: aligned, modelled and textured".into())),
                            _ => {}
                        },
                        _ => {}
                    }
                    send(Msg::Event(event));
                }
            }
            _ = tick.tick() => {
                match session.client.status().await {
                    Ok(status) => {
                        let idle = status.is_idle();
                        // Project loads run in the background and don't bump the change counter,
                        // so keep refreshing while the image count is still moving.
                        let loading = st.scene.as_ref().is_some_and(|sc| {
                            sc.images.len() as u32 != sc.project.image_count || sc.project.path.is_empty() && !sc.images.is_empty() && last_image_count != Some(sc.images.len())
                        });
                        if idle {
                            finish_exports_by_file(send, &mut st).await;
                            fail_stale_exports(send, &mut st);
                        }
                        if idle && !st.settling() && st.region_auto_pending.take().is_some() {
                            match session.client.send(&Command::new("setReconstructionRegionAuto").into()).await {
                                Ok(_) => {
                                    st.scene_stale = true;
                                    st.command_sent_at = Some(std::time::Instant::now());
                                }
                                Err(e) => send(Msg::Log(format!("Couldn't fit a reconstruction region: {e}"))),
                            }
                        }
                        // Reasons to re-read the state: the project changed, a load is in
                        // progress, a process just finished (the only signal without
                        // notifications), a coalesced refresh is owed, or a command went out.
                        let changed = last_change != Some(status.change_counter);
                        let need_refresh = changed
                            || loading
                            || (was_busy && idle)
                            || st.refresh_pending
                            || (idle && st.scene_stale)
                            || (idle && st.info_pending && st.last_refresh.is_none_or(|t| t.elapsed() > Duration::from_secs(2)));
                        let own_export_running = st.pending_mesh.is_some() || st.pending_region.is_some();
                        if need_refresh && own_export_running {
                            // Our export sequence may be mid-way through a temporary model;
                            // read the state once it is over.
                            st.refresh_pending = true;
                        } else if need_refresh {
                            tracing::debug!(
                                target: "rs",
                                "tick refresh: changed={changed} loading={loading} finished={} owed={} stale={} settling={}",
                                was_busy && idle,
                                st.refresh_pending,
                                st.scene_stale,
                                st.settling()
                            );
                            if st.settling() {
                                st.refresh_pending = true;
                            } else {
                                last_change = Some(status.change_counter);
                                refresh(&session.client, send, &mut st).await;
                                if !loading {
                                    sync_exports(&session.client, send, &mut st, false).await;
                                }
                            }
                        }
                        if st.sync_pending && idle && !st.settling() {
                            sync_exports(&session.client, send, &mut st, false).await;
                        }
                        last_image_count = st.scene.as_ref().map(|sc| sc.images.len());
                        was_busy = !idle;
                        if idle && !st.queued.is_empty() {
                            let queued = std::mem::take(&mut st.queued);
                            for cmd in queued {
                                handle_cmd(cmd, &session.client, send, &mut st).await;
                            }
                        }
                        send(Msg::Status(status));
                    }
                    // Only a dead connection ends the session; a bad reply is just logged.
                    Err(rs_client::Error::Http(e)) if e.is_connect() => {
                        send(Msg::Failed(format!("Lost contact with RealityScan: {e}")));
                        break Outcome::Lost;
                    }
                    Err(e) => send(Msg::Log(format!("Status query failed: {e}"))),
                }
                if let Some(line) = rs_log.as_deref().and_then(last_log_line)
                    && line != last_activity
                {
                    last_activity = line.clone();
                    send(Msg::Activity(line));
                }
            }
        }
    };

    match session.instance.take() {
        Some(instance) => {
            if matches!(outcome, Outcome::Quit) {
                send(Msg::Starting("Shutting RealityScan down…".into()));
            } else if matches!(outcome, Outcome::Relaunch) {
                send(Msg::Starting("Restarting RealityScan…".into()));
            }
            // After a lost connection this just cleans up whatever is left of the process tree.
            if let Err(e) = instance.shutdown(Duration::from_secs(60)).await {
                send(Msg::Log(format!("Shutdown problem: {e}")));
            }
        }
        None => {
            // Attached: leave RealityScan running; just stop it notifying us, if it's safe.
            if matches!(outcome, Outcome::Quit | Outcome::Relaunch)
                && session.events.is_some()
                && is_idle(&session.client).await
            {
                let _ = session.client.send(&Command::new("RsRemoteStopNotifyREST").into()).await;
            }
        }
    }
    outcome
}

async fn launch(install: Result<Install, rs_backend::InstallError>, send: &impl Fn(Msg)) -> Option<Session> {
    send(Msg::Starting("Looking for RealityScan…".into()));
    let install = match install {
        Ok(i) => i,
        Err(e) => {
            send(Msg::Failed(format!("RealityScan not found: {e}")));
            return None;
        }
    };
    send(Msg::Log(format!("Using {} ({})", install.exe_display(), install.label())));
    match rs_backend::autosave::pending(&install) {
        Ok(list) if !list.is_empty() => {
            send(Msg::Log(format!("{} pending autosave(s) left by an earlier RealityScan", list.len())));
            let infos = list
                .iter()
                .map(|a| {
                    let project_file = a.project_file();
                    let modified = project_file
                        .as_ref()
                        .and_then(|p| std::fs::metadata(p).ok())
                        .and_then(|m| m.modified().ok());
                    AutosaveInfo { project_file, modified }
                })
                .collect();
            send(Msg::Autosaves(infos));
        }
        Ok(_) => {}
        Err(e) => send(Msg::Log(format!("Couldn't read autosave list: {e}"))),
    }

    let launcher = Launcher::new(install.clone());
    // Wine's dxcore makes RealityScan's AI masking run on the CPU (a minute per image); ours
    // lets ONNX Runtime use the GPU through DirectML. Proton only: that's where it's verified.
    if matches!(install.runtime, rs_backend::Runtime::Proton { .. })
        && let Some(dll) = find_dxcore_dll()
    {
        match launcher.install_dxcore(&dll).await {
            Ok(true) => {
                send(Msg::Log(format!("Installed {} into the Wine prefix (GPU AI masking)", dll.display())))
            }
            Ok(false) => {}
            Err(e) => send(Msg::Log(format!("Couldn't install dxcore.dll into the Wine prefix: {e}"))),
        }
    }
    send(Msg::Starting("Starting RealityScan…".into()));
    let opts = LaunchOptions { instance_name: "realityscan-gtk".into(), ..LaunchOptions::default() };
    let mut instance = match launcher.launch(opts).await {
        Ok(i) => i,
        Err(e) => {
            send(Msg::Failed(format!("Couldn't start RealityScan: {e}")));
            return None;
        }
    };
    send(Msg::Log(format!("RealityScan log: {}", instance.log_path().display())));
    // Epic's Linux package can't read JPEG XL; borrow the decoder from the Heroic prefix.
    if let Some(heroic) = Install::detect_heroic().ok().filter(|_| install.distrobox().is_none()) {
        match launcher.install_jxl_codec(&heroic.drive_c().join("windows/system32")).await {
            Ok(true) => send(Msg::Log(
                "Installed the JPEG XL decoder from the Heroic prefix into the CrossOver bottle".into(),
            )),
            Ok(false) => {}
            Err(e) => {
                send(Msg::Log(format!("Couldn't install the JPEG XL decoder into the CrossOver bottle: {e}")))
            }
        }
    }
    let events = instance.take_events();
    Some(Session {
        client: instance.client().clone(),
        events,
        instance: Some(instance),
        install: Some(install),
    })
}

async fn attach(url: &str, send: &impl Fn(Msg)) -> Option<Session> {
    send(Msg::Starting(format!("Attaching to {url}…")));
    let client = Client::new(url);
    if !client.health().await {
        send(Msg::Failed(format!("Nothing answers at {url}")));
        return None;
    }
    // Enabling notifications is a command; sending it mid-process would abort that process.
    let events = if !is_idle(&client).await {
        send(Msg::Log("RealityScan is busy; attached without notifications (polling only)".into()));
        None
    } else {
        match EventReceiver::bind().await {
            Ok(rx) => {
                // Stop any previous target first; a second start doesn't reliably replace it.
                // Separately: with no previous target the stop fails and would abort a sequence.
                let _ = client.send(&Command::new("RsRemoteStopNotifyREST").into()).await;
                for _ in 0..20 {
                    if is_idle(&client).await {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(250)).await;
                }
                if let Err(e) =
                    client.send(&Command::new("RsRemoteStartNotifyREST").arg(rx.url()).into()).await
                {
                    send(Msg::Log(format!("Couldn't enable notifications: {e}")));
                }
                Some(rx)
            }
            Err(e) => {
                send(Msg::Log(format!("No event receiver: {e}")));
                None
            }
        }
    };
    send(Msg::Log(format!("Attached to running RealityScan at {url}")));
    Some(Session { client, events, instance: None, install: None })
}

/// Last non-empty line of a log file, reading only its tail.
fn last_log_line(path: &std::path::Path) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let start = len.saturating_sub(4096);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    text.lines().rev().map(str::trim).find(|l| !l.is_empty()).map(str::to_owned)
}

async fn recv_event(events: &mut Option<EventReceiver>) -> Option<Event> {
    match events {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

/// True when RealityScan reports no running or queued work. Unknown counts as busy.
async fn is_idle(client: &Client) -> bool {
    client.status().await.map(|s| s.is_idle()).unwrap_or(false)
}

/// Refresh unless one happened within the last second (events can arrive in bursts); a
/// skipped refresh is done on the next status tick instead.
async fn refresh_debounced(client: &Client, send: &impl Fn(Msg), st: &mut State) {
    let own_export_running = st.pending_mesh.is_some() || st.pending_region.is_some();
    if own_export_running
        || st.settling()
        || st.last_refresh.is_some_and(|t| t.elapsed() < Duration::from_secs(1))
    {
        st.refresh_pending = true;
        return;
    }
    refresh(client, send, st).await;
}

/// `idle` must come from a status read *before* any template evaluation of this refresh: a
/// template is itself a process, and the status reports busy for a moment after each one, so
/// a check made right after the state query always says busy.
async fn fetch_scene_with_info(
    client: &Client,
    st: &mut State,
    idle: bool,
) -> Result<Scene, rs_scene::Error> {
    let mut scene = rs_scene::fetch_scene(client).await?;
    let has_models = scene
        .project
        .selected_component()
        .and_then(|guid| scene.components.iter().find(|c| c.guid == guid))
        .is_some_and(|c| !c.models.is_empty());
    // The model-info template fails while a model is being calculated (the new model exists
    // but has no properties yet) and each failure is a failed process with its own error
    // event, so only ask when RealityScan is idle (background jobs included). Whatever was
    // known before is carried over meanwhile, and the tick asks again once idle.
    let may_ask = has_models && st.model_info_failed_at != Some(scene.project.change_count);
    let idle = may_ask && idle;
    tracing::debug!(
        target: "rs",
        "model info: has_models={has_models} may_ask={may_ask} idle={idle} selected={:?}",
        scene.project.selected_component()
    );
    if idle {
        match rs_scene::fetch_model_info(client).await {
            Ok(infos) => {
                tracing::debug!(target: "rs", "model info: {} entries, textured: {:?}", infos.len(), infos.iter().map(|i| (i.guid.as_str(), i.textured)).collect::<Vec<_>>());
                scene.merge_model_info(infos)
            }
            Err(e) => {
                tracing::warn!("model info query failed: {e}");
                st.model_info_failed_at = Some(scene.project.change_count);
                st.model_info_error = Some(e.to_string());
            }
        }
    }
    if let Some(old) = &st.scene {
        let known: Vec<rs_scene::ModelInfo> =
            old.components.iter().flat_map(|c| c.models.iter()).filter_map(|m| m.info.clone()).collect();
        for m in scene.components.iter_mut().flat_map(|c| c.models.iter_mut()) {
            if m.info.is_none()
                && let Some(info) = known.iter().find(|i| i.guid == m.guid)
            {
                m.info = Some(info.clone());
            }
        }
    }
    st.info_pending = may_ask
        && st.model_info_failed_at != Some(scene.project.change_count)
        && scene.components.iter().flat_map(|c| c.models.iter()).any(|m| m.info.is_none());
    Ok(scene)
}

async fn refresh(client: &Client, send: &impl Fn(Msg), st: &mut State) {
    st.last_refresh = Some(std::time::Instant::now());
    st.refresh_pending = false;
    st.scene_stale = false;
    // Read before the templates below run (see `fetch_scene_with_info`).
    let idle = is_idle(client).await;
    match rs_scene::controlpoints::fetch_control_points(client).await {
        Ok(data) => send(Msg::ControlPoints(data)),
        Err(e) => tracing::warn!("control point query failed: {e}"),
    }
    let key = match fetch_scene_with_info(client, st, idle).await {
        Ok(scene) => {
            if let Some(e) = st.model_info_error.take() {
                send(Msg::Log(format!(
                    "Model details (colours, texture) couldn't be read; they'll be retried when the project changes: {e}"
                )));
            }
            let key = scene
                .project
                .selected_component()
                .map(|guid| (guid.to_owned(), scene.selected_camera_count, scene.selected_point_count));
            st.scene = Some(scene.clone());
            send(Msg::Scene(scene));
            key
        }
        Err(e) => {
            send(Msg::Log(format!("State query failed: {e}")));
            return;
        }
    };
    // The ortho template fails ("There is no orthographic projection", a failed process with
    // its own error event) on a project without ortho projections and errors out without
    // components, so ask only while there may be some.
    let may_have_orthos =
        st.scene.as_ref().is_some_and(|s| !s.components.is_empty()) && !st.orthos_unavailable;
    if may_have_orthos {
        match rs_scene::ortho::fetch_orthos(client).await {
            Ok(list) => send(Msg::Orthos(list)),
            Err(e) => {
                tracing::debug!(target: "rs", "ortho projection query failed, not retrying until something changes: {e}");
                st.orthos_unavailable = true;
                send(Msg::Orthos(Vec::new()));
            }
        }
    } else if !st.orthos_unavailable {
        send(Msg::Orthos(Vec::new()));
    }
    if key == st.last_sparse {
        return;
    }
    if key.is_none() {
        st.last_sparse = None;
        send(Msg::Sparse { cloud: SparseCloud::default(), cameras: Vec::new() });
        if st.region_for.take().is_some() {
            send(Msg::Region(None));
        }
        return;
    }
    let (cloud, cameras) =
        tokio::join!(rs_scene::sparse::fetch_points(client), rs_scene::sparse::fetch_cameras(client));
    match (cloud, cameras) {
        (Ok(cloud), Ok(cameras)) => {
            st.last_sparse = key;
            send(Msg::Sparse { cloud, cameras });
        }
        (Err(e), _) | (_, Err(e)) => send(Msg::Log(format!("Sparse cloud query failed: {e}"))),
    }
}

/// The model to show: the selected one, else the first.
fn display_model(scene: &Scene) -> Option<&rs_scene::Model> {
    let models: Vec<_> = scene.components.iter().flat_map(|c| c.models.iter()).collect();
    scene
        .project
        .selected_model()
        .and_then(|guid| models.iter().find(|m| m.guid == guid))
        .or(models.first())
        .copied()
}

/// Export whatever the viewport is missing (reconstruction region, selected model) in one
/// sequence. Only acts while RealityScan is idle and nothing of ours is in flight.
async fn sync_exports(client: &Client, send: &impl Fn(Msg), st: &mut State, force_mesh: bool) {
    st.sync_pending = false;
    let Some(scene) = st.scene.clone() else { return };
    // A scene read before (or right after) our last command may name models that no longer
    // exist; the tick re-syncs once things have settled.
    if st.scene_stale
        || st.settling()
        || st.pending_mesh.is_some()
        || st.pending_region.is_some()
        || st.pending_export.is_some()
    {
        tracing::debug!(
            target: "rs",
            "sync skipped: stale={} settling={} pending mesh={} region={} export={}",
            st.scene_stale,
            st.settling(),
            st.pending_mesh.is_some(),
            st.pending_region.is_some(),
            st.pending_export.is_some()
        );
        return;
    }
    let mut seq = CommandSeq::new();
    let mut log = Vec::new();

    // Region: once per project change, for the selected component. These commands go after
    // the mesh export: a failure aborts the rest of a sequence, and a project aligned headless
    // has no region yet (see `fail_stale_exports`), which must not cost the mesh.
    let mut region_cmds = Vec::new();
    let mut region_job = None;
    match scene.project.selected_component() {
        Some(comp) => {
            let want = (comp.to_owned(), scene.project.change_count);
            if st.region_for.as_ref() != Some(&want) {
                let dir = cache_dir("regions");
                let path = dir.join(format!("{}.rsbox", file_stem(comp)));
                match std::fs::create_dir_all(&dir)
                    .map_err(|e| e.to_string())
                    .and_then(|_| to_wine(&path).map_err(|e| e.to_string()))
                {
                    Ok(wine_path) => {
                        region_cmds.push(Command::new("exportReconstructionRegion").arg(wine_path));
                        region_cmds.push(Command::tag("REGION_EXPORTED"));
                        region_job = Some((want.0, want.1, path));
                    }
                    Err(e) => send(Msg::Log(format!("Region export not possible: {e}"))),
                }
            }
        }
        None => {
            if st.region_for.take().is_some() {
                send(Msg::Region(None));
            }
        }
    }

    // Mesh: the selected (or first) model, unless already shown.
    let mut mesh_job = None;
    let mut cached_mesh = None;
    match display_model(&scene) {
        None => {
            if st.mesh_for.take().is_some() {
                send(Msg::Mesh(None));
            }
        }
        Some(model)
            if !force_mesh
                && st.mesh_for.as_deref() == Some(model.guid.as_str())
                && (st.mesh_textured || !model.is_textured()) => {}
        Some(model)
            if model.info.is_none() && st.model_info_failed_at != Some(scene.project.change_count) =>
        {
            // Whether the model is textured decides between a PLY and a (slow) textured GLB
            // export; deciding before the model's details are in meant a textured model came
            // up plain and was then exported a second time. The details arrive with the next
            // idle refresh, so wait for them (unless that query has failed for this project
            // state, in which case the plain export is all there is).
            tracing::debug!(target: "rs", "display export of {} waits for its model info", model.name);
            st.sync_pending = true;
        }
        Some(model) => {
            let dir = cache_dir("meshes");
            let limit =
                if model.is_textured() { max_textured_preview_triangles() } else { max_preview_triangles() };
            let oversized = model.triangles > limit;
            let ext = if model.is_textured() && !oversized { "glb" } else { "ply" };
            st.mesh_textured = model.is_textured() && !oversized;
            let path = dir.join(format!("{}.{ext}", file_stem(&model.guid)));
            if !force_mesh && path.is_file() {
                // Exported for display earlier (the file is removed when the model is
                // coloured, textured or edited): no need to ask RealityScan again.
                log.push(format!("Showing {} from the display cache", model.name));
                cached_mesh = Some((model.guid.clone(), path));
            } else {
                match std::fs::create_dir_all(&dir)
                    .map_err(|e| e.to_string())
                    .and_then(|_| to_wine(&path).map_err(|e| e.to_string()))
                {
                    Ok(wine_path) => {
                        seq.push(Command::new("selectModel").arg(&model.name));
                        if oversized {
                            // `simplify` makes a new, selected model: export that copy, delete it,
                            // and put the selection back.
                            let target = preview_target_triangles();
                            seq.push(Command::new("simplify").arg(target.to_string()));
                            seq.push(Command::new("exportSelectedModel").arg(wine_path));
                            seq.push(Command::new("deleteSelectedModel"));
                            seq.push(Command::new("selectModel").arg(&model.name));
                            log.push(format!(
                                "{} has {} triangles; making a temporary {}-triangle copy for display…",
                                model.name, model.triangles, target
                            ));
                        } else {
                            seq.push(Command::new("exportSelectedModel").arg(wine_path));
                            log.push(format!(
                                "Exporting {} ({} triangles{}) for display…",
                                model.name,
                                model.triangles,
                                if model.is_textured() { ", textured" } else { "" }
                            ));
                        }
                        seq.push(Command::tag("MESH_EXPORTED"));
                        mesh_job = Some((model.guid.clone(), path));
                    }
                    Err(e) => send(Msg::Log(format!("Mesh export not possible: {e}"))),
                }
            }
        }
    }
    if let Some((guid, path)) = cached_mesh {
        for l in log.drain(..) {
            send(Msg::Log(l));
        }
        st.pending_mesh = Some((guid, path));
        st.pending_since = Some(std::time::Instant::now());
        load_pending_mesh(send, st).await;
    }

    for cmd in region_cmds {
        seq.push(cmd);
    }
    if seq.is_empty() {
        return;
    }
    if !is_idle(client).await {
        // Brief busy blips (e.g. right after a delete) fall between status polls; the tick
        // retries this once RealityScan reports idle.
        tracing::debug!(target: "rs", "sync deferred: RealityScan busy");
        st.sync_pending = true;
        return;
    }
    match client.send(&seq).await {
        Ok(_) => {
            for l in log {
                send(Msg::Log(l));
            }
            st.pending_region = region_job;
            st.pending_mesh = mesh_job;
            st.pending_since = Some(std::time::Instant::now());
        }
        Err(e) => send(Msg::Log(format!("Export for display failed to start: {e}"))),
    }
}

/// Without notifications (attached while busy, or a lost `ON_TAG`), an export that did run
/// still shows as its output file: when RealityScan is idle and the file is newer than the
/// request, treat the job as done and load it.
async fn finish_exports_by_file(send: &impl Fn(Msg), st: &mut State) {
    let Some(since) = st.pending_since else { return };
    let fresh = |path: &PathBuf| {
        std::fs::metadata(path)
            .and_then(|m| m.modified())
            .is_ok_and(|m| m.elapsed().is_ok_and(|age| age <= since.elapsed()))
    };
    if st.pending_mesh.as_ref().is_some_and(|(_, path)| fresh(path)) {
        tracing::debug!(target: "rs", "mesh export finished (file present, no tag)");
        load_pending_mesh(send, st).await;
    }
    if st.pending_region.as_ref().is_some_and(|(_, _, path)| fresh(path)) {
        tracing::debug!(target: "rs", "region export finished (file present, no tag)");
        load_pending_region(send, st);
    }
    if let Some(path) = st.pending_export.clone()
        && rs_scene::ortho::ortho_output_candidates(&path).iter().any(fresh)
    {
        st.pending_export = None;
        let actual = rs_scene::ortho::ortho_output_candidates(&path).into_iter().find(|p| p.exists());
        send(Msg::Exported(actual.unwrap_or(path)));
    }
    if st.pending_mesh.is_none() && st.pending_region.is_none() && st.pending_export.is_none() {
        st.pending_since = None;
    }
}

/// Our export sequences report back through tags; if RealityScan is idle well after one was
/// sent and the tag never came, the sequence failed. Remember the jobs as done so they aren't
/// retried until the project changes (retrying at once would loop on a persistent failure).
fn fail_stale_exports(send: &impl Fn(Msg), st: &mut State) {
    let Some(since) = st.pending_since else { return };
    if since.elapsed() < Duration::from_secs(3) {
        return;
    }
    if let Some((guid, _)) = st.pending_mesh.take() {
        send(Msg::Log("The display export of the model failed; see RealityScan's log".into()));
        st.mesh_for = Some(guid);
    }
    if let Some((comp, change, _)) = st.pending_region.take() {
        if st.region_known.contains(&comp) {
            tracing::debug!(target: "rs", "region export failed; keeping the one shown");
        } else if st.region_auto_tried.as_deref() != Some(comp.as_str()) {
            // RealityScan's GUI fits a region after alignment; the headless engine doesn't, and
            // exporting a missing region fails with "Operation failed". Fit one: that bumps
            // the change count and the export runs again.
            send(Msg::Log("No reconstruction region yet; fitting one automatically".into()));
            st.region_auto_tried = Some(comp.clone());
            st.region_auto_pending = Some(comp.clone());
        } else {
            send(Msg::Log("The reconstruction region could not be exported".into()));
        }
        st.region_for = Some((comp, change));
    }
    if let Some(path) = st.pending_export.take() {
        send(Msg::Log(format!("Export to {} did not complete; see RealityScan's log", path.display())));
    }
    st.pending_since = None;
}

/// Image files in one folder that share a name (`image_045.jpg` and `image_045.jxl`) are, to
/// RealityScan, layers of a single image: it imports one photo per name and warns about the
/// rest. Say so up front, so a folder that imports short isn't a mystery.
fn layer_collisions(dir: &std::path::Path) -> Option<String> {
    const IMAGE_EXTS: [&str; 20] = [
        "jpg", "jpeg", "png", "tif", "tiff", "jxl", "heic", "heif", "webp", "bmp", "exr", "dng", "cr2",
        "cr3", "nef", "arw", "raf", "orf", "rw2", "pef",
    ];
    let mut by_stem: std::collections::BTreeMap<String, Vec<String>> = std::collections::BTreeMap::new();
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let p = entry.path();
        let is_image = p.is_file()
            && p.extension().is_some_and(|e| IMAGE_EXTS.iter().any(|x| e.eq_ignore_ascii_case(x)));
        if !is_image {
            continue;
        }
        let (Some(stem), Some(name)) = (p.file_stem(), p.file_name()) else { continue };
        by_stem
            .entry(stem.to_string_lossy().to_lowercase())
            .or_default()
            .push(name.to_string_lossy().into_owned());
    }
    let mut groups: Vec<Vec<String>> = by_stem.into_values().filter(|v| v.len() > 1).collect();
    for g in &mut groups {
        g.sort();
    }
    let first = groups.first()?;
    Some(format!(
        "{} image name{} in {} {} shared by several files (e.g. {}): RealityScan treats those as layers of one image and imports a single photo per name",
        groups.len(),
        if groups.len() == 1 { "" } else { "s" },
        dir.display(),
        if groups.len() == 1 { "is" } else { "are" },
        first.join(" / ")
    ))
}

/// Drop every cached display export: after a re-alignment, an alignment update or a component
/// change the models' coordinates may have moved while their GUIDs stayed, so nothing cached
/// can be trusted.
fn forget_all_display_files() {
    let Ok(dir) = std::fs::read_dir(cache_dir("meshes")) else { return };
    for entry in dir.flatten() {
        let _ = std::fs::remove_file(entry.path());
    }
}

/// Remove a model's display export (mesh, its info file and texture images) from the cache.
fn forget_display_files(guid: &str) {
    let stem = file_stem(guid);
    let Ok(dir) = std::fs::read_dir(cache_dir("meshes")) else { return };
    for entry in dir.flatten() {
        if entry.file_name().to_string_lossy().starts_with(&stem) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

fn file_stem(guid: &str) -> String {
    guid.chars().filter(|c| c.is_ascii_alphanumeric()).collect()
}

async fn load_pending_mesh(send: &impl Fn(Msg), st: &mut State) {
    let Some((guid, path)) = st.pending_mesh.take() else { return };
    let loaded = tokio::task::spawn_blocking(move || {
        let mut mesh = if path.extension().is_some_and(|e| e == "glb") {
            rs_scene::gltf::load_glb(&path)?
        } else {
            rs_scene::mesh::load_ply(&path)?
        };
        if !mesh.has_normals() {
            mesh.compute_normals();
        }
        Ok::<_, rs_scene::mesh::MeshError>(mesh)
    })
    .await;
    // The export sequence may have made and removed a temporary model; re-read the state.
    st.refresh_pending = true;
    match loaded {
        Ok(Ok(mesh)) => {
            st.mesh_for = Some(guid);
            send(Msg::Mesh(Some(Arc::new(mesh))));
        }
        Ok(Err(e)) => send(Msg::Log(format!("Couldn't read exported mesh: {e}"))),
        Err(e) => send(Msg::Log(format!("Mesh load task failed: {e}"))),
    }
}

fn load_pending_region(send: &impl Fn(Msg), st: &mut State) {
    let Some((comp, change, path)) = st.pending_region.take() else { return };
    // Either way this project state is dealt with; an unreadable file is not retried until
    // the project changes (it would be re-exported every tick otherwise).
    st.region_for = Some((comp.clone(), change));
    match rs_scene::region::load_rsbox(&path) {
        Ok(region) => {
            st.region_known.insert(comp);
            send(Msg::Region(Some(region)));
        }
        Err(e) => send(Msg::Log(format!("Couldn't read the reconstruction region: {e}"))),
    }
}

fn describe(cmd: &Cmd) -> &'static str {
    match cmd {
        Cmd::AddImages(_) => "adding images",
        Cmd::AddFolder(_) => "adding a folder",
        Cmd::Open(_) => "opening the project",
        Cmd::Save(_) => "saving",
        Cmd::SetImageFlag { .. } => "the image change",
        Cmd::Align => "alignment",
        Cmd::RunAll => "the workflow",
        Cmd::SelectComponent(_) | Cmd::RenameComponent { .. } | Cmd::DeleteComponent(_) => {
            "the component change"
        }
        Cmd::Model(_) => "model calculation",
        Cmd::SelectModel(_) | Cmd::RenameModel { .. } | Cmd::DeleteModel(_) | Cmd::DuplicateModel(_) => {
            "the model change"
        }
        Cmd::Colorize => "colorizing",
        Cmd::Texture => "texturing",
        Cmd::Simplify(_) => "simplification",
        Cmd::Smooth => "smoothing",
        Cmd::CloseHoles => "closing holes",
        Cmd::CleanModel => "cleaning",
        Cmd::KeepLargestPart => "removing small parts",
        Cmd::ExportModel { .. } => "the export",
        Cmd::SetRegion(_) | Cmd::RegionAuto | Cmd::RegionByDensity => "the region change",
        Cmd::SetSettings(_) => "the settings change",
        Cmd::ApplyMask { .. }
        | Cmd::RemoveMask(_)
        | Cmd::ImportMasks(_)
        | Cmd::ExportMasks(_)
        | Cmd::MaskFromMesh => "the mask change",
        Cmd::CalculateOrtho(_) => "the ortho projection",
        Cmd::ExportOrtho { .. } => "the ortho export",
        Cmd::AddMeasurement { .. }
        | Cmd::AddMeasurements(_)
        | Cmd::DeleteMeasurement { .. }
        | Cmd::MoveMeasurement { .. } => "the measurement change",
        Cmd::ImportGroundControl(_) | Cmd::ExportGroundControl(_) | Cmd::ExportMeasurements(_) => {
            "the control point transfer"
        }
        Cmd::SelectTriangles(_) => "the selection",
        Cmd::RemoveSelectedTriangles | Cmd::CutByRegion { .. } | Cmd::FillHoles | Cmd::ImportMesh { .. } => {
            "the mesh edit"
        }
        Cmd::GenerateMasks | Cmd::GenerateMasksFor(_) => "mask generation",
        Cmd::DraftAlign => "draft alignment",
        Cmd::DetectFeatures => "feature detection",
        Cmd::TextureOp(_) => "texturing",
        Cmd::ExportSparseCloud(_)
        | Cmd::ExportRegistration(_)
        | Cmd::ExportUndistorted(_)
        | Cmd::ExportXmp => "the export",
        Cmd::RenameControlPoint { .. } | Cmd::DeleteControlPoint(_) => "the control point change",
        Cmd::DefineDistance { .. } | Cmd::DeleteConstraint(_) => "the constraint change",
        Cmd::DetectMarkers => "marker detection",
        Cmd::UpdateAlignment => "the alignment update",
        Cmd::MergeComponents | Cmd::ExportComponent { .. } | Cmd::ImportComponent(_) => {
            "the component change"
        }
        Cmd::ShowMesh => "mesh export",
        Cmd::Abort
        | Cmd::Refresh
        | Cmd::Relaunch
        | Cmd::SetRuntime(_)
        | Cmd::DiscardAutosaves
        | Cmd::Quit => "that command",
    }
}

/// The `dxcore.dll` built from `tools/dxcore`: `REALITYSCAN_GTK_DXCORE`, the user's or the
/// system's `share/realityscan-gtk/`, next to the binary's `share/`, or the source tree.
fn find_dxcore_dll() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("REALITYSCAN_GTK_DXCORE") {
        let p = PathBuf::from(p);
        return p.is_file().then_some(p);
    }
    let mut candidates = Vec::new();
    if let Some(data) = dirs::data_dir() {
        candidates.push(data.join("realityscan-gtk/dxcore.dll"));
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(prefix) = exe.parent().and_then(|bin| bin.parent())
    {
        candidates.push(prefix.join("share/realityscan-gtk/dxcore.dll"));
    }
    candidates.push(PathBuf::from("/usr/local/share/realityscan-gtk/dxcore.dll"));
    candidates.push(PathBuf::from("/usr/share/realityscan-gtk/dxcore.dll"));
    candidates.push(PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../tools/dxcore/dxcore.dll")));
    candidates.into_iter().find(|p| p.is_file())
}

/// The runtime to use: the `REALITYSCAN_GTK_RUNTIME` override, else the saved preference
/// (looked up among `all`, or resolved on its own if it isn't there), else the first found.
fn pick_install(all: &[Install], preference: Option<&str>) -> Result<Install, rs_backend::InstallError> {
    if let Ok(spec) = std::env::var(rs_backend::RUNTIME_ENV) {
        return Install::from_spec(&spec);
    }
    match preference {
        Some(spec) => match all.iter().find(|i| i.spec() == spec) {
            Some(i) => Ok(i.clone()),
            None => Install::from_spec(spec),
        },
        None => all.first().cloned().ok_or(rs_backend::InstallError::NotFound),
    }
}

/// Detect the runtimes on this machine, tell the UI about them, and pick the one to launch.
async fn choose_install(send: &impl Fn(Msg)) -> Result<Install, rs_backend::InstallError> {
    send(Msg::Starting("Looking for RealityScan…".into()));
    let preference = crate::settings::get(crate::settings::RUNTIME_KEY);
    let (all, chosen) = tokio::task::spawn_blocking(move || {
        let all = Install::detect_all();
        let chosen = pick_install(&all, preference.as_deref());
        (all, chosen)
    })
    .await
    .unwrap_or_else(|_| (Vec::new(), Err(rs_backend::InstallError::NotFound)));
    if all.is_empty() {
        send(Msg::Log("No RealityScan runtime found".into()));
    } else {
        send(Msg::Log(format!(
            "RealityScan runtimes: {}",
            all.iter().map(|i| format!("{} [{}]", i.label(), i.spec())).collect::<Vec<_>>().join("; ")
        )));
    }
    send(Msg::Runtimes {
        available: all.iter().map(|i| (i.spec(), i.label())).collect(),
        current: chosen.as_ref().ok().map(|i| i.spec()),
    });
    chosen
}

/// Clear RealityScan's autosave entries from the prefix registry.
async fn discard_autosaves(send: &impl Fn(Msg)) {
    let preference = crate::settings::get(crate::settings::RUNTIME_KEY);
    let install =
        tokio::task::spawn_blocking(move || pick_install(&Install::detect_all(), preference.as_deref()))
            .await
            .unwrap_or(Err(rs_backend::InstallError::NotFound));
    let install = match install {
        Ok(i) => i,
        Err(e) => {
            send(Msg::Log(format!("Can't find the RealityScan install: {e}")));
            return;
        }
    };
    match rs_backend::autosave::pending(&install) {
        Ok(list) if list.is_empty() => send(Msg::Log("No autosaves to discard".into())),
        Ok(list) => match rs_backend::autosave::clear(&Launcher::new(install.clone()), &list).await {
            Ok(()) => send(Msg::Log(format!("Discarded {} autosave(s)", list.len()))),
            Err(e) => send(Msg::Log(format!("Couldn't clear the autosave entries: {e}"))),
        },
        Err(e) => send(Msg::Log(format!("Couldn't read the autosave list: {e}"))),
    }
}

async fn handle_cmd(cmd: Cmd, client: &Client, send: &impl Fn(Msg), st: &mut State) {
    match cmd {
        Cmd::Abort => {
            if let Err(e) = client.abort().await {
                send(Msg::Log(format!("Abort failed: {e}")));
            }
            return;
        }
        Cmd::Refresh => {
            st.last_sparse = None;
            st.region_for = None;
            refresh(client, send, st).await;
            sync_exports(client, send, st, false).await;
            return;
        }
        Cmd::Quit | Cmd::SetRuntime(_) => unreachable!("handled by the loop"),
        Cmd::Relaunch => {
            send(Msg::Log("RealityScan is already running".into()));
            return;
        }
        Cmd::DiscardAutosaves => {
            discard_autosaves(send).await;
            return;
        }
        _ => {}
    }
    if !is_idle(client).await {
        // Sending now would make RealityScan abort what it's doing; hold the command instead.
        send(Msg::Log(format!("RealityScan is busy; {} queued until it is idle", describe(&cmd))));
        st.queued.push(cmd);
        return;
    }
    let result = match cmd {
        Cmd::AddImages(paths) => {
            let mut seq = CommandSeq::new();
            for p in &paths {
                match to_wine(p) {
                    Ok(w) => {
                        seq.push(Command::add(w));
                    }
                    Err(e) => send(Msg::Log(format!("Skipping {}: {e}", p.display()))),
                }
            }
            if seq.is_empty() {
                return;
            }
            send(Msg::Log(format!("Adding {} image(s)", seq.commands().len())));
            client.send(&seq).await.map(|_| ())
        }
        Cmd::AddFolder(path) => match to_wine(&path) {
            Ok(w) => {
                send(Msg::Log(format!("Adding folder {}", path.display())));
                if let Some(note) = layer_collisions(&path) {
                    send(Msg::Log(note));
                }
                client.send(&Command::add_folder(w).into()).await.map(|_| ())
            }
            Err(e) => {
                send(Msg::Log(format!("Skipping {}: {e}", path.display())));
                return;
            }
        },
        Cmd::Open(path) => match to_wine(&path) {
            Ok(w) => {
                send(Msg::Log(format!("Opening {}", path.display())));
                st.mesh_for = None;
                st.region_for = None;
                st.orthos_unavailable = false;
                forget_all_display_files();
                client.send(&Command::load(w).into()).await.map(|_| ())
            }
            Err(e) => {
                send(Msg::Log(format!("Can't open {}: {e}", path.display())));
                return;
            }
        },
        Cmd::Save(None) => {
            send(Msg::Log("Saving project…".into()));
            client.send(&Command::new("save").into()).await.map(|_| ())
        }
        Cmd::Save(Some(path)) => {
            // Saving onto the file that's already open makes RealityScan close and reopen the
            // project (losing unsaved work on the way); a bare save writes it in place.
            let current = st.scene.as_ref().and_then(|sc| {
                let p = rs_client::path::from_wine(&sc.project.path, None)?;
                Some(p.with_extension("rsproj"))
            });
            if current.as_deref() == Some(path.as_path()) {
                send(Msg::Log("Saving project…".into()));
                client.send(&Command::new("save").into()).await.map(|_| ())
            } else {
                match to_wine(&path) {
                    Ok(w) => {
                        send(Msg::Log(format!("Saving to {}", path.display())));
                        client.send(&Command::save(w).into()).await.map(|_| ())
                    }
                    Err(e) => {
                        send(Msg::Log(format!("Can't save to {}: {e}", path.display())));
                        return;
                    }
                }
            }
        }
        Cmd::SetImageFlag { images, flag, enabled } => {
            if images.is_empty() {
                return;
            }
            // `selectImage` matches the path exactly as the project stores it.
            let mut seq = CommandSeq::new();
            seq.push(Command::new("deselectAllImages"));
            for (i, img) in images.iter().enumerate() {
                seq.push(Command::new("selectImage").arg(img).arg(if i == 0 { "set" } else { "union" }));
            }
            seq.push(Command::new(flag.command()).arg(if enabled { "true" } else { "false" }));
            seq.push(Command::new("deselectAllImages"));
            send(Msg::Log(format!(
                "{} {} image(s) for {}",
                if enabled { "Enabling" } else { "Disabling" },
                images.len(),
                flag.label()
            )));
            client.send(&seq).await.map(|_| ())
        }
        Cmd::Align => {
            send(Msg::Log("Aligning…".into()));
            // A re-alignment can move the component's frame under its models.
            st.mesh_for = None;
            forget_all_display_files();
            client
                .send(&CommandSeq::new().then(Command::align()).then(Command::tag("ALIGN_DONE")))
                .await
                .map(|_| ())
        }
        Cmd::SelectComponent(name) => {
            send(Msg::Log(format!("Selecting component {name}")));
            st.mesh_for = None;
            client.send(&Command::new("selectComponent").arg(name).into()).await.map(|_| ())
        }
        Cmd::RenameComponent { component, new_name } => {
            send(Msg::Log(format!("Renaming component {component} to {new_name}")));
            client
                .send(
                    &CommandSeq::new()
                        .then(Command::new("selectComponent").arg(component))
                        .then(Command::new("renameSelectedComponent").arg(new_name)),
                )
                .await
                .map(|_| ())
        }
        Cmd::DeleteComponent(component) => {
            send(Msg::Log(format!("Deleting component {component}")));
            st.mesh_for = None;
            client
                .send(
                    &CommandSeq::new()
                        .then(Command::new("selectComponent").arg(component))
                        .then(Command::new("deleteSelectedComponent")),
                )
                .await
                .map(|_| ())
        }
        Cmd::RunAll => {
            let fresh = st.scene.as_ref().is_none_or(|s| s.components.is_empty());
            send(Msg::Log(format!(
                "Running the whole workflow: align{}, model at normal detail, texture…",
                if fresh { ", automatic region" } else { "" }
            )));
            st.region_for = None;
            st.mesh_for = None;
            forget_all_display_files();
            let mut seq = CommandSeq::new().then(Command::align()).then(Command::tag("ALIGN_DONE"));
            if fresh {
                seq = seq.then(Command::set_reconstruction_region_auto());
            }
            let seq = seq
                .then(Command::calculate_normal_model())
                .then(Command::tag("MODEL_DONE"))
                .then(Command::calculate_texture())
                .then(Command::tag("TEXTURE_DONE"))
                .then(Command::tag("RUN_DONE"));
            client.send(&seq).await.map(|_| ())
        }
        Cmd::Model(detail) => {
            let (label, calc) = match detail {
                Detail::Preview => ("preview", Command::calculate_preview_model()),
                Detail::Normal => ("normal", Command::calculate_normal_model()),
                Detail::High => ("high", Command::calculate_high_model()),
            };
            send(Msg::Log(format!("Calculating model ({label} detail)…")));
            client.send(&CommandSeq::new().then(calc).then(Command::tag("MODEL_DONE"))).await.map(|_| ())
        }
        Cmd::SelectModel(name) => {
            send(Msg::Log(format!("Selecting model {name}")));
            client.send(&Command::new("selectModel").arg(name).into()).await.map(|_| ())
        }
        Cmd::RenameModel { model, new_name } => {
            send(Msg::Log(format!("Renaming model {model} to {new_name}")));
            client
                .send(
                    &CommandSeq::new()
                        .then(Command::new("selectModel").arg(model))
                        .then(Command::new("renameSelectedModel").arg(new_name)),
                )
                .await
                .map(|_| ())
        }
        Cmd::DeleteModel(model) => {
            send(Msg::Log(format!("Deleting model {model}")));
            // Only re-export if the deleted model is the one on screen.
            let shown = st.scene.as_ref().and_then(|sc| {
                sc.components
                    .iter()
                    .flat_map(|c| c.models.iter())
                    .find(|m| m.name == model)
                    .map(|m| m.guid.clone())
            });
            if shown.is_some() && shown == st.mesh_for {
                st.mesh_for = None;
            }
            client
                .send(
                    &CommandSeq::new()
                        .then(Command::new("selectModel").arg(model))
                        .then(Command::new("deleteSelectedModel")),
                )
                .await
                .map(|_| ())
        }
        Cmd::DuplicateModel(model) => {
            send(Msg::Log(format!("Duplicating model {model}")));
            client
                .send(
                    &CommandSeq::new()
                        .then(Command::new("selectModel").arg(model))
                        .then(Command::new("duplicateSelectedModel")),
                )
                .await
                .map(|_| ())
        }
        Cmd::Colorize => {
            send(Msg::Log("Calculating vertex colours…".into()));
            client
                .send(
                    &CommandSeq::new()
                        .then(Command::new("calculateVertexColors"))
                        .then(Command::tag("COLOR_DONE")),
                )
                .await
                .map(|_| ())
        }
        Cmd::Texture => {
            send(Msg::Log("Texturing…".into()));
            client
                .send(
                    &CommandSeq::new().then(Command::calculate_texture()).then(Command::tag("TEXTURE_DONE")),
                )
                .await
                .map(|_| ())
        }
        Cmd::Simplify(target) => {
            send(Msg::Log(format!("Simplifying to about {target} triangles…")));
            client
                .send(
                    &CommandSeq::new()
                        .then(Command::new("simplify").arg(target.to_string()))
                        .then(Command::tag("MESH_EDITED")),
                )
                .await
                .map(|_| ())
        }
        Cmd::Smooth => {
            send(Msg::Log("Smoothing…".into()));
            client
                .send(&CommandSeq::new().then(Command::new("smooth")).then(Command::tag("MESH_EDITED")))
                .await
                .map(|_| ())
        }
        Cmd::CloseHoles => {
            send(Msg::Log("Closing holes…".into()));
            client
                .send(&CommandSeq::new().then(Command::new("closeHoles")).then(Command::tag("MESH_EDITED")))
                .await
                .map(|_| ())
        }
        Cmd::CleanModel => {
            send(Msg::Log("Cleaning the model…".into()));
            client
                .send(&CommandSeq::new().then(Command::new("cleanModel")).then(Command::tag("MESH_EDITED")))
                .await
                .map(|_| ())
        }
        Cmd::KeepLargestPart => {
            send(Msg::Log("Removing everything but the largest connected part…".into()));
            client
                .send(
                    &CommandSeq::new()
                        .then(Command::new("selectLargestModelComponent"))
                        .then(Command::new("invertTrianglesSelection"))
                        .then(Command::new("removeSelectedTriangles"))
                        .then(Command::tag("MESH_EDITED")),
                )
                .await
                .map(|_| ())
        }
        Cmd::ExportModel { model, path, mut options } => {
            // Asking for textures from a model that has none crashed RealityScan 2.2.
            let textured = st
                .scene
                .as_ref()
                .map(|sc| {
                    sc.components
                        .iter()
                        .flat_map(|c| c.models.iter())
                        .any(|m| m.name == model && m.is_textured())
                })
                .unwrap_or(false);
            if !textured {
                options.textures = false;
                options.embed_textures = false;
                options.one_texture = false;
            }
            let dir = cache_dir("settings");
            let xml = dir.join("export.xml");
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("obj");
            let prepared = std::fs::create_dir_all(&dir)
                .and_then(|_| std::fs::write(&xml, options.to_xml(ext)))
                .map_err(|e| e.to_string())
                .and_then(|_| {
                    Ok((
                        to_wine(&path).map_err(|e| e.to_string())?,
                        to_wine(&xml).map_err(|e| e.to_string())?,
                    ))
                });
            match prepared {
                Ok((wine_path, wine_xml)) => {
                    send(Msg::Log(format!("Exporting {model} to {}…", path.display())));
                    st.pending_export = Some(path);
                    st.pending_since = Some(std::time::Instant::now());
                    client
                        .send(
                            &CommandSeq::new()
                                .then(Command::new("selectModel").arg(model))
                                .then(Command::new("exportSelectedModel").arg(wine_path).arg(wine_xml))
                                .then(Command::tag("EXPORT_DONE")),
                        )
                        .await
                        .map(|_| ())
                }
                Err(e) => {
                    send(Msg::Log(format!("Can't export to {}: {e}", path.display())));
                    return;
                }
            }
        }
        Cmd::SetRegion(region) => {
            let dir = cache_dir("regions");
            let path = dir.join("set.rsbox");
            let prepared = std::fs::create_dir_all(&dir)
                .and_then(|_| std::fs::write(&path, region.to_rsbox()))
                .map_err(|e| e.to_string())
                .and_then(|_| to_wine(&path).map_err(|e| e.to_string()));
            match prepared {
                Ok(wine_path) => {
                    send(Msg::Log("Setting the reconstruction region".into()));
                    st.region_for = None;
                    client
                        .send(&Command::new("setReconstructionRegion").arg(wine_path).into())
                        .await
                        .map(|_| ())
                }
                Err(e) => {
                    send(Msg::Log(format!("Can't write the region file: {e}")));
                    return;
                }
            }
        }
        Cmd::SetSettings(pairs) => {
            if pairs.is_empty() {
                return;
            }
            let mut seq = CommandSeq::new();
            for (k, v) in &pairs {
                seq.push(Command::set(k, v));
            }
            send(Msg::Log(format!(
                "Applying settings: {}",
                pairs.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(", ")
            )));
            client.send(&seq).await.map(|_| ())
        }
        Cmd::AddMeasurements(rows) => {
            let dir = cache_dir("measurements");
            let path = dir.join("add.csv");
            let csv = rs_scene::controlpoints::measurements_csv(&rows);
            let prepared = std::fs::create_dir_all(&dir)
                .and_then(|_| std::fs::write(&path, csv))
                .map_err(|e| e.to_string())
                .and_then(|_| to_wine(&path).map_err(|e| e.to_string()));
            match prepared {
                Ok(wine_path) => {
                    let images: Vec<&str> = rows.iter().map(|r| r.0.as_str()).collect();
                    send(Msg::Log(format!(
                        "Placing {} on {} from the 3D view",
                        rows.first().map(|r| r.1.as_str()).unwrap_or("?"),
                        images.join(", ")
                    )));
                    client
                        .send(&Command::new("importControlPointsMeasurements").arg(wine_path).into())
                        .await
                        .map(|_| ())
                }
                Err(e) => {
                    send(Msg::Log(format!("Can't write the measurement file: {e}")));
                    return;
                }
            }
        }
        Cmd::AddMeasurement { point, image, x, y } => {
            // Measurements can only be imported from a CSV (`Image, Point, X, Y`).
            let dir = cache_dir("measurements");
            let path = dir.join("add.csv");
            let csv = rs_scene::controlpoints::measurements_csv(&[(image.clone(), point.clone(), x, y)]);
            let prepared = std::fs::create_dir_all(&dir)
                .and_then(|_| std::fs::write(&path, csv))
                .map_err(|e| e.to_string())
                .and_then(|_| to_wine(&path).map_err(|e| e.to_string()));
            match prepared {
                Ok(wine_path) => {
                    send(Msg::Log(format!("Placing {point} on {image} at {x:.1}, {y:.1}")));
                    client
                        .send(&Command::new("importControlPointsMeasurements").arg(wine_path).into())
                        .await
                        .map(|_| ())
                }
                Err(e) => {
                    send(Msg::Log(format!("Can't write the measurement file: {e}")));
                    return;
                }
            }
        }
        Cmd::MoveMeasurement { point, index, image, x, y } => {
            let dir = cache_dir("measurements");
            let path = dir.join("move.csv");
            let csv = rs_scene::controlpoints::measurements_csv(&[(image.clone(), point.clone(), x, y)]);
            let prepared = std::fs::create_dir_all(&dir)
                .and_then(|_| std::fs::write(&path, csv))
                .map_err(|e| e.to_string())
                .and_then(|_| to_wine(&path).map_err(|e| e.to_string()));
            match prepared {
                Ok(wine_path) => {
                    send(Msg::Log(format!("Moving {point} on {image} to {x:.1}, {y:.1}")));
                    client
                        .send(
                            &CommandSeq::new()
                                .then(
                                    Command::new("selectMeasurementByIndex")
                                        .arg(&point)
                                        .arg(index.to_string()),
                                )
                                .then(Command::new("deleteControlPointMeasurement"))
                                .then(Command::new("importControlPointsMeasurements").arg(wine_path)),
                        )
                        .await
                        .map(|_| ())
                }
                Err(e) => {
                    send(Msg::Log(format!("Can't write the measurement file: {e}")));
                    return;
                }
            }
        }
        Cmd::ImportGroundControl(path) => match to_wine(&path) {
            Ok(w) => {
                send(Msg::Log(format!("Importing ground control points from {}", path.display())));
                client.send(&Command::new("importGroundControlPoints").arg(w).into()).await.map(|_| ())
            }
            Err(e) => {
                send(Msg::Log(format!("Can't import {}: {e}", path.display())));
                return;
            }
        },
        Cmd::ExportGroundControl(_)
        | Cmd::ExportMeasurements(_)
        | Cmd::ExportSparseCloud(_)
        | Cmd::ExportRegistration(_) => {
            let (what, command, path) = match cmd {
                Cmd::ExportGroundControl(p) => ("ground control points", "exportGroundControlPoints", p),
                Cmd::ExportMeasurements(p) => {
                    ("control point measurements", "exportControlPointsMeasurements", p)
                }
                Cmd::ExportSparseCloud(p) => ("the sparse point cloud", "exportSparsePointCloud", p),
                Cmd::ExportRegistration(p) => ("the registration", "exportRegistration", p),
                _ => unreachable!(),
            };
            match to_wine(&path) {
                Ok(w) => {
                    send(Msg::Log(format!("Exporting {what} to {}…", path.display())));
                    st.pending_export = Some(path);
                    st.pending_since = Some(std::time::Instant::now());
                    client
                        .send(
                            &CommandSeq::new()
                                .then(Command::new(command).arg(w))
                                .then(Command::tag("EXPORT_DONE")),
                        )
                        .await
                        .map(|_| ())
                }
                Err(e) => {
                    send(Msg::Log(format!("Can't export to {}: {e}", path.display())));
                    return;
                }
            }
        }
        Cmd::SelectTriangles(selector) => {
            send(Msg::Log(format!("Selecting triangles: {selector:?}")));
            client.send(&selector.command().into()).await.map(|_| ())
        }
        Cmd::ImportMesh { mesh, keep } => {
            let kept = keep.iter().filter(|k| **k).count();
            let total = mesh.0.triangle_count();
            send(Msg::Log(format!(
                "Making a new model with {kept} of {total} triangles (the current model stays)…"
            )));
            let dir = cache_dir("edits");
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or_default();
            let path = dir.join(format!("edit-{stamp}.ply"));
            let written = tokio::task::spawn_blocking({
                let path = path.clone();
                move || {
                    std::fs::create_dir_all(path.parent().expect("edit path has a parent"))?;
                    rs_scene::mesh::write_ply(&mesh.0.subset(&keep), &path)
                }
            })
            .await;
            let wine_path = match written {
                Ok(Ok(())) => match to_wine(&path) {
                    Ok(w) => w,
                    Err(e) => {
                        send(Msg::Log(format!("Can't hand {} to RealityScan: {e}", path.display())));
                        return;
                    }
                },
                Ok(Err(e)) => {
                    send(Msg::Log(format!("Couldn't write the edited mesh: {e}")));
                    return;
                }
                Err(e) => {
                    send(Msg::Log(format!("Couldn't write the edited mesh: {e}")));
                    return;
                }
            };
            st.mesh_for = None;
            client
                .send(
                    &CommandSeq::new()
                        .then(Command::new("importModel").arg(wine_path))
                        .then(Command::tag("MESH_EDITED")),
                )
                .await
                .map(|_| ())
        }
        Cmd::RemoveSelectedTriangles => {
            send(Msg::Log("Removing the selected triangles (makes a new model)…".into()));
            client
                .send(
                    &CommandSeq::new()
                        .then(Command::new("removeSelectedTriangles"))
                        .then(Command::tag("MESH_EDITED")),
                )
                .await
                .map(|_| ())
        }
        Cmd::CutByRegion { keep_inside, fill_holes } => {
            // `inner` filters out what is inside the region, `outer` what is outside.
            let which = if keep_inside { "outer" } else { "inner" };
            send(Msg::Log(format!(
                "Cutting the model by the reconstruction region (keeping the {}; makes a new model)…",
                if keep_inside { "inside" } else { "outside" }
            )));
            client
                .send(
                    &CommandSeq::new()
                        .then(Command::new("cutByBox").arg(which).arg(if fill_holes {
                            "true"
                        } else {
                            "false"
                        }))
                        .then(Command::tag("MESH_EDITED")),
                )
                .await
                .map(|_| ())
        }
        Cmd::FillHoles => {
            send(Msg::Log("Filling holes…".into()));
            client
                .send(&CommandSeq::new().then(Command::new("fillHoles")).then(Command::tag("MESH_EDITED")))
                .await
                .map(|_| ())
        }
        Cmd::GenerateMasksFor(images) => {
            let mut seq = CommandSeq::new();
            for (i, img) in images.iter().enumerate() {
                seq.push(Command::new("selectImage").arg(img).arg(if i == 0 { "set" } else { "union" }));
            }
            seq.push(Command::new("generateAIMasks"));
            seq.push(Command::new("deselectAllImages"));
            seq.push(Command::tag("MASKS_DONE"));
            send(Msg::Log(format!("Generating AI masks for {} image(s) (about 40 s each)…", images.len())));
            client.send(&seq).await.map(|_| ())
        }
        Cmd::GenerateMasks => {
            send(Msg::Log("Generating AI masks for all images…".into()));
            client
                .send(
                    &CommandSeq::new().then(Command::new("generateAIMasks")).then(Command::tag("MASKS_DONE")),
                )
                .await
                .map(|_| ())
        }
        Cmd::DraftAlign => {
            st.mesh_for = None;
            forget_all_display_files();
            send(Msg::Log("Draft alignment…".into()));
            client
                .send(&CommandSeq::new().then(Command::new("draft")).then(Command::tag("ALIGN_DONE")))
                .await
                .map(|_| ())
        }
        Cmd::DetectFeatures => {
            send(Msg::Log("Detecting features…".into()));
            client
                .send(
                    &CommandSeq::new()
                        .then(Command::new("detectFeatures"))
                        .then(Command::tag("FEATURES_DONE")),
                )
                .await
                .map(|_| ())
        }
        Cmd::TextureOp(op) => {
            let (label, command, tag) = match op {
                TextureOp::Unwrap => ("Unwrapping the model…", "unwrap", "TEXTURE_DONE"),
                TextureOp::QualityTexture => {
                    ("Calculating a quality texture…", "calculateQualityTexture", "TEXTURE_DONE")
                }
                TextureOp::CorrectColors => ("Correcting colours…", "correctColors", "COLOR_DONE"),
                TextureOp::PreviewColors => {
                    ("Calculating preview vertex colours…", "calculatePreviewVertexColors", "COLOR_DONE")
                }
                TextureOp::QualityColors => {
                    ("Calculating quality vertex colours…", "calculateQualityColors", "COLOR_DONE")
                }
            };
            send(Msg::Log(label.into()));
            client
                .send(&CommandSeq::new().then(Command::new(command)).then(Command::tag(tag)))
                .await
                .map(|_| ())
        }
        Cmd::ExportUndistorted(folder) => match to_wine(&folder) {
            Ok(w) => {
                send(Msg::Log(format!("Exporting undistorted images to {}…", folder.display())));
                st.pending_export = Some(folder);
                st.pending_since = Some(std::time::Instant::now());
                client
                    .send(
                        &CommandSeq::new()
                            .then(Command::new("exportUndistortedImages").arg(w))
                            .then(Command::tag("EXPORT_DONE")),
                    )
                    .await
                    .map(|_| ())
            }
            Err(e) => {
                send(Msg::Log(format!("Can't export to {}: {e}", folder.display())));
                return;
            }
        },
        Cmd::ApplyMask { image_index, path } => match to_wine(&path) {
            Ok(w) => {
                send(Msg::Log(format!("Setting the mask of image {image_index} from {}", path.display())));
                client
                    .send(
                        &Command::new("setImageLayer").arg(image_index.to_string()).arg(w).arg("mask").into(),
                    )
                    .await
                    .map(|_| ())
            }
            Err(e) => {
                send(Msg::Log(format!("Can't use {}: {e}", path.display())));
                return;
            }
        },
        Cmd::RemoveMask(image) => {
            send(Msg::Log(format!("Removing the mask of {image}")));
            client
                .send(
                    &CommandSeq::new()
                        .then(Command::new("selectImage").arg(image).arg("set"))
                        .then(Command::new("removeImageLayer").arg("mask"))
                        .then(Command::new("deselectAllImages")),
                )
                .await
                .map(|_| ())
        }
        Cmd::ImportMasks(folder) => {
            let Some(scene) = st.scene.as_ref() else { return };
            let mut seq = CommandSeq::new();
            let mut found = 0;
            for img in &scene.images {
                let candidates = ["png", "jpg", "jpeg", "tif", "tiff", "bmp"].iter().flat_map(|ext| {
                    [
                        folder.join(format!("{}.{ext}", img.name)),
                        folder.join(format!("{}.mask.{ext}", img.name)),
                        folder.join(format!("{}_mask.{ext}", img.name)),
                    ]
                });
                if let Some(mask) = candidates.into_iter().find(|p| p.is_file())
                    && let Ok(w) = to_wine(&mask)
                {
                    seq.push(Command::new("setImageLayer").arg(img.index.to_string()).arg(w).arg("mask"));
                    found += 1;
                }
            }
            if found == 0 {
                send(Msg::Log(format!("No mask files matching the image names in {}", folder.display())));
                return;
            }
            send(Msg::Log(format!("Setting masks for {found} image(s) from {}", folder.display())));
            client.send(&seq).await.map(|_| ())
        }
        Cmd::ExportMasks(folder) => match to_wine(&folder) {
            Ok(w) => {
                send(Msg::Log(format!("Exporting masks to {}…", folder.display())));
                st.pending_export = Some(folder);
                st.pending_since = Some(std::time::Instant::now());
                client
                    .send(
                        &CommandSeq::new()
                            .then(Command::new("exportMasks").arg(w))
                            .then(Command::tag("EXPORT_DONE")),
                    )
                    .await
                    .map(|_| ())
            }
            Err(e) => {
                send(Msg::Log(format!("Can't export to {}: {e}", folder.display())));
                return;
            }
        },
        Cmd::MaskFromMesh => {
            send(Msg::Log("Generating masks from the selected model…".into()));
            client
                .send(
                    &CommandSeq::new()
                        .then(Command::new("generateMaskFromMesh"))
                        .then(Command::tag("MASKS_DONE")),
                )
                .await
                .map(|_| ())
        }
        Cmd::CalculateOrtho(req) => {
            st.orthos_unavailable = false;
            let dir = cache_dir("ortho");
            let path = dir.join("request.rsortho");
            let prepared = std::fs::create_dir_all(&dir)
                .and_then(|_| std::fs::write(&path, req.to_rsortho()))
                .map_err(|e| e.to_string())
                .and_then(|_| to_wine(&path).map_err(|e| e.to_string()));
            match prepared {
                Ok(w) => {
                    let (pw, ph) = req.pixel_size();
                    send(Msg::Log(format!("Calculating ortho projection {} ({pw}×{ph} px)…", req.name)));
                    client
                        .send(
                            &CommandSeq::new()
                                .then(Command::new("calculateOrthoProjection").arg(w))
                                .then(Command::tag("ORTHO_DONE")),
                        )
                        .await
                        .map(|_| ())
                }
                Err(e) => {
                    send(Msg::Log(format!("Can't write the projection file: {e}")));
                    return;
                }
            }
        }
        Cmd::ExportOrtho { name, path } => {
            let dir = cache_dir("settings");
            let xml = dir.join("ortho-export.xml");
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("tiff").to_owned();
            // RealityScan adds the extension itself.
            let base = path.with_extension("");
            let prepared = std::fs::create_dir_all(&dir)
                .and_then(|_| std::fs::write(&xml, rs_scene::ortho::export_xml(&ext)))
                .map_err(|e| e.to_string())
                .and_then(|_| {
                    Ok((
                        to_wine(&base).map_err(|e| e.to_string())?,
                        to_wine(&xml).map_err(|e| e.to_string())?,
                    ))
                });
            match prepared {
                Ok((w, wx)) => {
                    send(Msg::Log(format!("Exporting ortho projection {name} to {}…", path.display())));
                    st.pending_export = Some(path);
                    st.pending_since = Some(std::time::Instant::now());
                    client
                        .send(
                            &CommandSeq::new()
                                .then(Command::new("exportOrthoProjection").arg(name).arg(w).arg(wx))
                                .then(Command::tag("EXPORT_DONE")),
                        )
                        .await
                        .map(|_| ())
                }
                Err(e) => {
                    send(Msg::Log(format!("Can't export to {}: {e}", path.display())));
                    return;
                }
            }
        }
        Cmd::ExportXmp => {
            send(Msg::Log("Writing XMP camera files next to the images…".into()));
            client
                .send(&CommandSeq::new().then(Command::new("exportXMP")).then(Command::tag("XMP_DONE")))
                .await
                .map(|_| ())
        }
        Cmd::RenameControlPoint { point, new_name } => {
            send(Msg::Log(format!("Renaming control point {point} to {new_name}")));
            client.send(&Command::new("renameControlPoint").arg(point).arg(new_name).into()).await.map(|_| ())
        }
        Cmd::DeleteControlPoint(index) => {
            send(Msg::Log(format!("Deleting control point {index}")));
            client.send(&Command::new("deleteControlPoint").arg(index.to_string()).into()).await.map(|_| ())
        }
        Cmd::DeleteMeasurement { point, index } => {
            send(Msg::Log(format!("Deleting measurement {index} of {point}")));
            client
                .send(
                    &CommandSeq::new()
                        .then(Command::new("selectMeasurementByIndex").arg(point).arg(index.to_string()))
                        .then(Command::new("deleteControlPointMeasurement")),
                )
                .await
                .map(|_| ())
        }
        Cmd::DefineDistance { a, b, distance, name } => {
            send(Msg::Log(format!("Defining distance {a} – {b} = {distance}")));
            let mut cmd = Command::new("defineDistance").arg(a).arg(b).arg(distance.to_string());
            if let Some(n) = name {
                cmd = cmd.arg(n);
            }
            client.send(&cmd.into()).await.map(|_| ())
        }
        Cmd::DeleteConstraint(index) => {
            send(Msg::Log(format!("Deleting distance constraint {index}")));
            client.send(&Command::new("deleteConstraint").arg(index.to_string()).into()).await.map(|_| ())
        }
        Cmd::DetectMarkers => {
            send(Msg::Log("Detecting markers in the images…".into()));
            client
                .send(
                    &CommandSeq::new().then(Command::new("detectMarkers")).then(Command::tag("MARKERS_DONE")),
                )
                .await
                .map(|_| ())
        }
        Cmd::UpdateAlignment => {
            send(Msg::Log("Updating the alignment to fit the control points and constraints…".into()));
            st.mesh_for = None;
            st.last_sparse = None;
            forget_all_display_files();
            client
                .send(&CommandSeq::new().then(Command::new("update")).then(Command::tag("UPDATE_DONE")))
                .await
                .map(|_| ())
        }
        Cmd::MergeComponents => {
            send(Msg::Log("Merging components…".into()));
            st.mesh_for = None;
            forget_all_display_files();
            client
                .send(
                    &CommandSeq::new().then(Command::new("mergeComponents")).then(Command::tag("MERGE_DONE")),
                )
                .await
                .map(|_| ())
        }
        Cmd::ExportComponent { component, path } => match to_wine(&path) {
            Ok(w) => {
                send(Msg::Log(format!("Exporting component {component} to {}", path.display())));
                st.pending_export = Some(path);
                st.pending_since = Some(std::time::Instant::now());
                client
                    .send(
                        &CommandSeq::new()
                            .then(Command::new("selectComponent").arg(component))
                            .then(Command::new("exportSelectedComponentFile").arg(w))
                            .then(Command::tag("EXPORT_DONE")),
                    )
                    .await
                    .map(|_| ())
            }
            Err(e) => {
                send(Msg::Log(format!("Can't export to {}: {e}", path.display())));
                return;
            }
        },
        Cmd::ImportComponent(path) => match to_wine(&path) {
            Ok(w) => {
                send(Msg::Log(format!("Importing component {}", path.display())));
                client.send(&Command::new("importComponent").arg(w).into()).await.map(|_| ())
            }
            Err(e) => {
                send(Msg::Log(format!("Can't import {}: {e}", path.display())));
                return;
            }
        },
        Cmd::RegionAuto => {
            send(Msg::Log("Setting the reconstruction region automatically".into()));
            st.region_for = None;
            client.send(&Command::set_reconstruction_region_auto().into()).await.map(|_| ())
        }
        Cmd::RegionByDensity => {
            send(Msg::Log("Setting the reconstruction region by point density".into()));
            st.region_for = None;
            client.send(&Command::new("setReconstructionRegionByDensity").into()).await.map(|_| ())
        }
        Cmd::ShowMesh => {
            sync_exports(client, send, st, true).await;
            Ok(())
        }
        Cmd::Abort
        | Cmd::Refresh
        | Cmd::Relaunch
        | Cmd::SetRuntime(_)
        | Cmd::DiscardAutosaves
        | Cmd::Quit => {
            unreachable!()
        }
    };
    match result {
        Ok(()) => {
            st.scene_stale = true;
            st.command_sent_at = Some(std::time::Instant::now());
        }
        Err(e) => send(Msg::Log(format!("Command failed: {e}"))),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn layer_collisions_names_duplicate_image_stems() {
        let dir = std::env::temp_dir().join(format!("rsgtk-layers-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["image_045.jpg", "image_045.jxl", "image_046.jxl", "image_046.xmp", "Thumbs.db"] {
            std::fs::write(dir.join(name), b"x").unwrap();
        }
        let note = super::layer_collisions(&dir).expect("one collision");
        assert!(note.starts_with("1 image name in "), "{note}");
        assert!(note.contains("image_045.jpg / image_045.jxl"), "{note}");
        std::fs::remove_file(dir.join("image_045.jpg")).unwrap();
        assert!(super::layer_collisions(&dir).is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
