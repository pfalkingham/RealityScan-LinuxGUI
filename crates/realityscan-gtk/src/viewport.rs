//! 3D viewport: a `GtkGLArea` driven by `rs_render`, with an overlay toolbar.
//!
//! Mouse: left-drag orbits, middle- or right-drag pans, wheel zooms, a left click
//! on a camera selects it (reported through [`Viewport::connect_camera_clicked`]).

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;

use gtk4 as gtk;

use glam::{Mat3, Mat4, Vec3};
use gtk::glib;
use gtk::prelude::*;
use rs_render::{MeshPart, OrbitCamera, Pose, Renderer, Segment, TextureUpload};
use rs_scene::mesh::Mesh;
use rs_scene::region::Region;
use rs_scene::sparse::{CameraPose, SparseCloud};

/// Click tolerance for picking a camera, in logical pixels.
const PICK_RADIUS: f64 = 14.0;
const CAMERA_COLOR: [u8; 3] = [255, 200, 60];
const SELECTED_CAMERA_COLOR: [u8; 3] = [255, 255, 255];
const REGION_COLOR: [u8; 3] = [80, 200, 255];
const CONTROL_POINT_COLOR: [u8; 3] = [255, 90, 220];
const HANDLE_COLOR: [u8; 3] = [160, 230, 255];
const HOVER_COLOR: [u8; 3] = [255, 255, 255];

type CameraClicked = Rc<RefCell<Option<Box<dyn Fn(Option<u32>)>>>>;
type CameraActivated = Rc<RefCell<Option<Box<dyn Fn(u32)>>>>;
type RegionChanged = Rc<RefCell<Option<Box<dyn Fn(Region)>>>>;
type SelectionChanged = Rc<RefCell<Option<Box<dyn Fn(usize)>>>>;
/// A surface point picked with Ctrl+click: position and the normal there.
type PointPicked = Rc<RefCell<Option<Box<dyn Fn(Vec3, Vec3)>>>>;
type CamerasLassoed = Rc<RefCell<Option<Box<dyn Fn(Vec<u32>, LassoMode)>>>>;
/// The displayed mesh and, per triangle, whether the edit keeps it.
type EditRequested = Rc<RefCell<Option<Box<dyn Fn(Arc<Mesh>, Vec<bool>)>>>>;

/// How a finished lasso combines with the existing selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LassoMode {
    Replace,
    /// Shift held.
    Add,
    /// Ctrl held.
    Subtract,
}

/// A draggable part of the reconstruction-region gizmo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Handle {
    /// Centre of the face on the `axis` (0 = X, 1 = Y, 2 = Z of the box), on its positive or
    /// negative side; dragging it moves that face.
    Face { axis: usize, positive: bool },
    /// Box centre; dragging it moves the whole box in the view plane.
    Centre,
}

struct RegionDrag {
    handle: Handle,
    start: Region,
    start_pointer: (f64, f64),
    /// Outward world direction of the dragged face.
    axis: Vec3,
    /// Screen movement (pixels) per world unit along `axis`.
    px_per_unit: (f64, f64),
    /// World units per pixel at the box centre, for moving the box.
    world_per_px: f32,
    right: Vec3,
    up: Vec3,
}

#[derive(Clone)]
pub struct Viewport {
    root: gtk::Overlay,
    area: gtk::GLArea,
    state: Rc<RefCell<State>>,
    points_toggle: gtk::ToggleButton,
    mesh_toggle: gtk::ToggleButton,
    texture_toggle: gtk::ToggleButton,
    cameras_toggle: gtk::ToggleButton,
    region_toggle: gtk::ToggleButton,
    on_camera_clicked: CameraClicked,
    on_camera_activated: CameraActivated,
    on_region_changed: RegionChanged,
    lasso_toggle: gtk::ToggleButton,
    /// Transparent layer over the GL area that draws the lasso outline.
    lasso_layer: gtk::DrawingArea,
    /// Floating bar with the selected-triangle count and the edits.
    selection_bar: gtk::Box,
    selection_label: gtk::Label,
    on_selection_changed: SelectionChanged,
    on_cameras_lassoed: CamerasLassoed,
    on_edit: EditRequested,
    on_point_picked: PointPicked,
}

/// A camera as drawn: RealityScan index, centre, pose and frustum depth.
struct CameraView {
    index: u32,
    position: Vec3,
    pose: Pose,
}

struct State {
    renderer: Option<Renderer>,
    camera: OrbitCamera,
    // Scene content (CPU side); `dirty_*` marks what the GPU still needs.
    positions: Vec<Vec3>,
    colors: Vec<[u8; 3]>,
    cameras: Vec<CameraView>,
    frustum_depth: f32,
    selected_cameras: HashSet<u32>,
    axes: Vec<Segment>,
    /// Edge and handle segments of the region box (derived from `region_model`).
    region: Vec<Segment>,
    region_model: Option<Region>,
    region_drag: Option<RegionDrag>,
    hover: Option<Handle>,
    /// Triangulated control points: (name, position, selected).
    control_points: Vec<Segment>,
    mesh: Option<Arc<Mesh>>,
    dirty_points: bool,
    dirty_lines: bool,
    dirty_mesh: bool,
    show_points: bool,
    show_mesh: bool,
    show_textures: bool,
    show_colors: bool,
    show_cameras: bool,
    show_region: bool,
    /// Bounds of the sparse scene, for framing.
    focus: Option<(Vec3, Vec3)>,
    last_pointer: (f64, f64),
    /// Where the current drag started and how far it moved (to tell clicks from drags).
    drag_moved: f64,
    /// World point under the pointer when the left drag began: the orbit pivot. `None` means
    /// the framing centre (background under the pointer, or no render yet).
    pivot: Option<Vec3>,
    /// Pointer position whose depth the next render samples to find the pivot.
    pending_pick: Option<(f64, f64)>,
    /// Ctrl+click position: the next render turns it into a surface point for the app.
    pending_point_pick: Option<(f64, f64)>,
    /// Orbit deltas that arrived before the pivot was known.
    pending_orbit: (f32, f32),
    /// Lasso tool: a left drag outlines a selection instead of orbiting.
    lasso_mode: bool,
    /// Outline being drawn, in logical pixels.
    lasso: Vec<(f64, f64)>,
    /// A finished outline, resolved against the depth buffer on the next render.
    lasso_pending: Option<(Vec<(f64, f64)>, LassoMode)>,
    /// Per triangle of `mesh`: lasso-selected.
    selected: Vec<bool>,
    selected_count: usize,
    dirty_selection: bool,
}

impl Default for State {
    fn default() -> Self {
        Self {
            renderer: None,
            // RealityScan's default world has gravity along -Z.
            camera: OrbitCamera { up: Vec3::Z, ..OrbitCamera::default() },
            positions: Vec::new(),
            colors: Vec::new(),
            cameras: Vec::new(),
            frustum_depth: 0.1,
            selected_cameras: HashSet::new(),
            axes: Vec::new(),
            region: Vec::new(),
            region_model: None,
            region_drag: None,
            hover: None,
            control_points: Vec::new(),
            mesh: None,
            dirty_points: true,
            dirty_lines: true,
            dirty_mesh: true,
            show_points: true,
            show_mesh: true,
            show_textures: true,
            show_colors: true,
            show_cameras: true,
            show_region: true,
            focus: None,
            last_pointer: (0.0, 0.0),
            drag_moved: 0.0,
            pivot: None,
            pending_pick: None,
            pending_point_pick: None,
            pending_orbit: (0.0, 0.0),
            lasso_mode: false,
            lasso: Vec::new(),
            lasso_pending: None,
            selected: Vec::new(),
            selected_count: 0,
            dirty_selection: false,
        }
    }
}

/// Two bounding boxes that overlap and are within a factor of three in size: the same object.
fn similar_extent(a: ([f32; 3], [f32; 3]), b: ([f32; 3], [f32; 3])) -> bool {
    let (alo, ahi, blo, bhi) = (Vec3::from(a.0), Vec3::from(a.1), Vec3::from(b.0), Vec3::from(b.1));
    let (da, db) = ((ahi - alo).length().max(1e-6), (bhi - blo).length().max(1e-6));
    let centres = ((ahi + alo) - (bhi + blo)).length() * 0.5;
    centres < da.max(db) * 0.5 && (da / db).max(db / da) < 3.0
}

/// Normal of the mesh vertex nearest to `p` (unit, or +Z when the mesh has no normals).
fn nearest_normal(mesh: &Mesh, p: Vec3) -> Vec3 {
    let mut best = (f32::MAX, 0usize);
    for (i, v) in mesh.positions.iter().enumerate() {
        let d = (Vec3::from(*v) - p).length_squared();
        if d < best.0 {
            best = (d, i);
        }
    }
    mesh.normals
        .get(best.1)
        .map(|n| Vec3::from(*n))
        .filter(|n| n.length() > 0.0)
        .map_or(Vec3::Z, |n| n.normalize())
}

/// Even-odd test of a point against a polygon (logical pixels).
fn inside_polygon(poly: &[(f64, f64)], x: f64, y: f64) -> bool {
    let mut inside = false;
    let n = poly.len();
    let mut j = n.wrapping_sub(1);
    for i in 0..n {
        let (xi, yi) = poly[i];
        let (xj, yj) = poly[j];
        if (yi > y) != (yj > y) && x < (xj - xi) * (y - yi) / (yj - yi) + xi {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// View-space distance for a depth-buffer value under the perspective used for rendering.
fn linear_depth(z_window: f32, near: f32, far: f32) -> f32 {
    let ndc = z_window * 2.0 - 1.0;
    2.0 * near * far / ((far + near) - ndc * (far - near))
}

/// Vertex indices (three per triangle) of the selected triangles.
fn selected_indices(mesh: &Mesh, selected: &[bool]) -> Vec<u32> {
    mesh.indices
        .as_chunks::<3>()
        .0
        .iter()
        .zip(selected)
        .filter(|(_, s)| **s)
        .flat_map(|(tri, _)| tri.iter().copied())
        .collect()
}

/// World directions of the box's local axes.
fn region_axes(r: &Region) -> [Vec3; 3] {
    let m = r.rotation();
    [0, 1, 2].map(|k| Vec3::new(m[0][k] as f32, m[1][k] as f32, m[2][k] as f32))
}

fn region_centre(r: &Region) -> Vec3 {
    Vec3::new(r.centre[0] as f32, r.centre[1] as f32, r.centre[2] as f32)
}

/// Handles with their world positions: six face centres and the box centre.
fn region_handles(r: &Region) -> Vec<(Handle, Vec3)> {
    let axes = region_axes(r);
    let c = region_centre(r);
    let mut out = Vec::with_capacity(7);
    for (axis, dir) in axes.iter().enumerate() {
        for positive in [true, false] {
            let sign = if positive { 0.5 } else { -0.5 };
            out.push((Handle::Face { axis, positive }, c + *dir * (r.size[axis] as f32 * sign)));
        }
    }
    out.push((Handle::Centre, c));
    out
}

/// Edges of the region box plus its handles (a small square per face, a cross at the centre).
fn region_segments(r: &Region, hover: Option<Handle>, with_handles: bool) -> Vec<Segment> {
    let corners = r.corners().map(|c| Vec3::new(c[0] as f32, c[1] as f32, c[2] as f32));
    let mut out: Vec<Segment> = Region::EDGES
        .iter()
        .map(|&(a, b)| Segment { a: corners[a], b: corners[b], color: REGION_COLOR })
        .collect();
    if !with_handles {
        return out;
    }
    let axes = region_axes(r);
    let diag = (r.size[0] * r.size[0] + r.size[1] * r.size[1] + r.size[2] * r.size[2]).sqrt() as f32;
    let h = (diag * 0.025).max(1e-3);
    for (handle, pos) in region_handles(r) {
        let color = if hover == Some(handle) { HOVER_COLOR } else { HANDLE_COLOR };
        match handle {
            Handle::Face { axis, .. } => {
                let (u, v) = (axes[(axis + 1) % 3] * h, axes[(axis + 2) % 3] * h);
                let q = [pos + u + v, pos - u + v, pos - u - v, pos + u - v];
                for i in 0..4 {
                    out.push(Segment { a: q[i], b: q[(i + 1) % 4], color });
                }
            }
            Handle::Centre => {
                for a in axes {
                    out.push(Segment { a: pos - a * h, b: pos + a * h, color });
                }
            }
        }
    }
    out
}

fn rebuild_region(st: &mut State) {
    st.region = match &st.region_model {
        Some(r) => region_segments(r, st.hover, true),
        None => Vec::new(),
    };
    st.dirty_lines = true;
}

/// World point to logical pixels, if in front of the eye.
fn to_pixels(vp: &Mat4, p: Vec3, w: f64, h: f64) -> Option<(f64, f64)> {
    let (nx, ny, _) = rs_render::project(vp, p)?;
    Some(((nx as f64 + 1.0) * 0.5 * w, (1.0 - ny as f64) * 0.5 * h))
}

/// Frustum outlines for all cameras, selected ones brighter and larger.
fn frusta(cameras: &[CameraView], selected_cameras: &HashSet<u32>, frustum_depth: f32) -> Vec<Segment> {
    let mut out = Vec::with_capacity(cameras.len() * 9);
    for cam in cameras {
        let selected = selected_cameras.contains(&cam.index);
        let color = if selected { SELECTED_CAMERA_COLOR } else { CAMERA_COLOR };
        let depth = if selected { frustum_depth * 1.6 } else { frustum_depth };
        out.extend(rs_render::frustum_lines(&cam.pose, depth, color));
    }
    out
}

/// The parts of the viewport state a lasso resolution reads and writes.
struct LassoInput<'a> {
    camera: &'a OrbitCamera,
    cameras: &'a [CameraView],
    /// The mesh, when it is shown.
    mesh: Option<&'a Arc<Mesh>>,
    selected: &'a mut Vec<bool>,
    selected_count: &'a mut usize,
    dirty_selection: &'a mut bool,
}

/// Resolve a finished outline. With a mesh on screen the visible triangles inside it are
/// selected (per `mode`) and `None` is returned; otherwise the cameras inside it are. Runs
/// inside the render callback, right after drawing, so the depth buffer is current.
fn resolve_lasso(
    input: LassoInput<'_>,
    renderer: &Renderer,
    outline: &[(f64, f64)],
    mode: LassoMode,
    w: f64,
    h: f64,
    scale: i32,
) -> Option<Vec<u32>> {
    if outline.len() < 3 || w <= 0.0 || h <= 0.0 {
        return None;
    }
    let vp = input.camera.view_projection((w / h) as f32);
    let (min_x, max_x) = outline.iter().fold((f64::MAX, f64::MIN), |(lo, hi), p| (lo.min(p.0), hi.max(p.0)));
    let (min_y, max_y) = outline.iter().fold((f64::MAX, f64::MIN), |(lo, hi), p| (lo.min(p.1), hi.max(p.1)));
    let in_outline = |px: f64, py: f64| {
        px >= min_x && px <= max_x && py >= min_y && py <= max_y && inside_polygon(outline, px, py)
    };

    let Some(mesh) = input.mesh else {
        let hits: Vec<u32> = input
            .cameras
            .iter()
            .filter(|cam| to_pixels(&vp, cam.position, w, h).is_some_and(|(px, py)| in_outline(px, py)))
            .map(|cam| cam.index)
            .collect();
        return Some(hits);
    };

    let (dw, dh) = (w as i32 * scale, h as i32 * scale);
    let depth = renderer.read_depth_buffer(dw, dh);
    let near = (input.camera.distance * 0.001).max(1e-4);
    let far = input.camera.distance * 100.0;
    let selected = input.selected;
    selected.resize(mesh.triangle_count(), false);
    if mode == LassoMode::Replace {
        selected.iter_mut().for_each(|s| *s = false);
    }
    for (t, tri) in mesh.indices.as_chunks::<3>().0.iter().enumerate() {
        let [a, b, c] = [
            mesh.positions[tri[0] as usize],
            mesh.positions[tri[1] as usize],
            mesh.positions[tri[2] as usize],
        ];
        let centroid = Vec3::new(a[0] + b[0] + c[0], a[1] + b[1] + c[1], a[2] + b[2] + c[2]) / 3.0;
        let Some((nx, ny, nz)) = rs_render::project(&vp, centroid) else { continue };
        let (px, py) = ((nx as f64 + 1.0) * 0.5 * w, (1.0 - ny as f64) * 0.5 * h);
        if !in_outline(px, py) {
            continue;
        }
        // Only the surface the viewer sees: compare with what was drawn at that pixel.
        let dx = ((px * scale as f64) as i32).clamp(0, dw - 1);
        let dy = (dh - 1 - (py * scale as f64) as i32).clamp(0, dh - 1);
        let drawn = linear_depth(depth[(dy * dw + dx) as usize], near, far);
        let own = linear_depth(nz * 0.5 + 0.5, near, far);
        if own > drawn * 1.003 {
            continue;
        }
        selected[t] = mode != LassoMode::Subtract;
    }
    *input.selected_count = selected.iter().filter(|s| **s).count();
    *input.dirty_selection = true;
    None
}

impl Viewport {
    pub fn new() -> Self {
        let area = gtk::GLArea::builder().has_depth_buffer(true).hexpand(true).vexpand(true).build();
        area.set_required_version(3, 3);
        let root = gtk::Overlay::builder().child(&area).build();

        // Overlay toolbar
        let bar = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(3)
            .halign(gtk::Align::End)
            .valign(gtk::Align::Start)
            .margin_top(10)
            .margin_end(10)
            .build();
        bar.add_css_class("toolbar");
        bar.add_css_class("osd");
        let reset = gtk::Button::from_icon_name("zoom-fit-best-symbolic");
        reset.set_tooltip_text(Some("Reset view"));
        let points_toggle = gtk::ToggleButton::builder().label("Points").active(true).build();
        points_toggle.set_tooltip_text(Some("Show the sparse point cloud"));
        let mesh_toggle = gtk::ToggleButton::builder().label("Mesh").active(true).sensitive(false).build();
        mesh_toggle.set_tooltip_text(Some("Show the reconstructed mesh"));
        let texture_toggle =
            gtk::ToggleButton::builder().label("Texture").active(true).visible(false).build();
        texture_toggle.set_tooltip_text(Some("Show the texture instead of vertex colours"));
        let cameras_toggle = gtk::ToggleButton::builder().label("Cameras").active(true).build();
        cameras_toggle.set_tooltip_text(Some("Show camera positions"));
        let region_toggle = gtk::ToggleButton::builder().label("Region").active(true).build();
        region_toggle.set_tooltip_text(Some("Show the reconstruction region"));
        let lasso_toggle = gtk::ToggleButton::builder().icon_name("edit-select-all-symbolic").build();
        lasso_toggle.set_tooltip_text(Some(
            "Lasso: drag around triangles (mesh) or cameras (points) to select them.\nShift adds, Ctrl removes, a click clears.",
        ));
        bar.append(&reset);
        for t in [&points_toggle, &mesh_toggle, &texture_toggle, &cameras_toggle, &region_toggle] {
            bar.append(t);
        }
        bar.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        bar.append(&lasso_toggle);
        root.add_overlay(&bar);

        // The lasso outline is drawn on a transparent layer that lets events through.
        let lasso_layer = gtk::DrawingArea::builder().can_target(false).hexpand(true).vexpand(true).build();
        root.add_overlay(&lasso_layer);

        // Selected-triangle count and what to do with them; shown while something is selected.
        let selection_bar = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::End)
            .margin_bottom(12)
            .visible(false)
            .build();
        selection_bar.add_css_class("toolbar");
        selection_bar.add_css_class("osd");
        let selection_label = gtk::Label::new(None);
        selection_label.set_margin_start(6);
        let remove_button = gtk::Button::with_label("Remove");
        remove_button.set_tooltip_text(Some("New model without the selected triangles"));
        remove_button.add_css_class("destructive-action");
        let keep_button = gtk::Button::with_label("Keep only");
        keep_button.set_tooltip_text(Some("New model with just the selected triangles"));
        let invert_button = gtk::Button::with_label("Invert");
        let clear_button = gtk::Button::from_icon_name("window-close-symbolic");
        clear_button.set_tooltip_text(Some("Clear the selection"));
        selection_bar.append(&selection_label);
        for b in [&remove_button, &keep_button, &invert_button, &clear_button] {
            selection_bar.append(b);
        }
        root.add_overlay(&selection_bar);

        let state = Rc::new(RefCell::new(State::default()));
        let this = Self {
            root,
            area,
            state,
            points_toggle,
            mesh_toggle,
            texture_toggle,
            cameras_toggle: cameras_toggle.clone(),
            region_toggle: region_toggle.clone(),
            on_camera_clicked: Rc::new(RefCell::new(None)),
            on_camera_activated: Rc::new(RefCell::new(None)),
            on_region_changed: Rc::new(RefCell::new(None)),
            lasso_toggle,
            lasso_layer,
            selection_bar,
            selection_label,
            on_selection_changed: Rc::new(RefCell::new(None)),
            on_cameras_lassoed: Rc::new(RefCell::new(None)),
            on_edit: Rc::new(RefCell::new(None)),
            on_point_picked: Rc::new(RefCell::new(None)),
        };

        this.lasso_toggle.connect_toggled({
            let this = this.clone();
            move |t| {
                let on = t.is_active();
                this.state.borrow_mut().lasso_mode = on;
                this.area.set_cursor_from_name(Some(if on { "crosshair" } else { "default" }));
            }
        });
        this.lasso_layer.set_draw_func({
            let this = this.clone();
            move |_, cr, _, _| {
                let st = this.state.borrow();
                if st.lasso.len() < 2 {
                    return;
                }
                cr.move_to(st.lasso[0].0, st.lasso[0].1);
                for &(x, y) in &st.lasso[1..] {
                    cr.line_to(x, y);
                }
                cr.close_path();
                cr.set_source_rgba(1.0, 0.55, 0.1, 0.15);
                let _ = cr.fill_preserve();
                cr.set_source_rgba(1.0, 1.0, 1.0, 0.95);
                cr.set_line_width(1.5);
                cr.set_dash(&[6.0, 4.0], 0.0);
                let _ = cr.stroke();
            }
        });
        remove_button.connect_clicked({
            let this = this.clone();
            move |_| this.edit_selection(false)
        });
        keep_button.connect_clicked({
            let this = this.clone();
            move |_| this.edit_selection(true)
        });
        invert_button.connect_clicked({
            let this = this.clone();
            move |_| this.invert_selection()
        });
        clear_button.connect_clicked({
            let this = this.clone();
            move |_| this.clear_selection()
        });

        this.area.connect_realize({
            let this = this.clone();
            move |area| {
                area.make_current();
                if let Some(e) = area.error() {
                    tracing::error!("GL context error: {e}");
                    return;
                }
                match Renderer::new(load_gl()) {
                    Ok(r) => {
                        let mut st = this.state.borrow_mut();
                        st.renderer = Some(r);
                        st.dirty_points = true;
                        st.dirty_lines = true;
                        st.dirty_mesh = true;
                    }
                    Err(e) => tracing::error!("renderer init failed: {e}"),
                }
            }
        });
        this.area.connect_unrealize({
            let this = this.clone();
            move |area| {
                area.make_current();
                this.state.borrow_mut().renderer = None;
            }
        });
        this.area.connect_render({
            let this = this.clone();
            move |area, _ctx| {
                let mut st = this.state.borrow_mut();
                let st = &mut *st;
                if let Some(renderer) = st.renderer.as_mut() {
                    if st.dirty_points {
                        renderer.set_points(&st.positions, &st.colors);
                        st.dirty_points = false;
                    }
                    if st.dirty_lines {
                        let mut lines = st.axes.clone();
                        if st.show_cameras {
                            lines.extend(frusta(&st.cameras, &st.selected_cameras, st.frustum_depth));
                        }
                        if st.show_region {
                            lines.extend_from_slice(&st.region);
                        }
                        lines.extend_from_slice(&st.control_points);
                        renderer.set_lines(&lines);
                        st.dirty_lines = false;
                    }
                    if st.dirty_mesh {
                        match &st.mesh {
                            Some(m) => {
                                let parts: Vec<MeshPart> = m
                                    .parts
                                    .iter()
                                    .map(|p| MeshPart {
                                        first_index: p.first_index,
                                        index_count: p.index_count,
                                        texture: p.texture,
                                    })
                                    .collect();
                                renderer.set_mesh(
                                    &m.positions,
                                    &m.normals,
                                    &m.colors,
                                    &m.uvs,
                                    &m.indices,
                                    &parts,
                                );
                                let textures: Vec<TextureUpload<'_>> = m
                                    .textures
                                    .iter()
                                    .map(|t| TextureUpload {
                                        width: t.width,
                                        height: t.height,
                                        rgba: &t.rgba,
                                    })
                                    .collect();
                                renderer.set_textures(&textures);
                            }
                            None => renderer.clear_mesh(),
                        }
                        st.dirty_mesh = false;
                    }
                    if st.dirty_selection {
                        let indices = st.mesh.as_ref().map(|m| selected_indices(m, &st.selected)).unwrap_or_default();
                        tracing::debug!(target: "rs", "selection: {} triangles highlighted", indices.len() / 3);
                        renderer.set_selection(&indices);
                        st.dirty_selection = false;
                    }
                    renderer.show_points = st.show_points;
                    renderer.show_mesh = st.show_mesh;
                    renderer.show_textures = st.show_textures;
                    renderer.show_colors = st.show_colors;
                    let scale = area.scale_factor();
                    let (w, h) = (area.width() as f64, area.height() as f64);
                    renderer.render(area.width() * scale, area.height() * scale, &st.camera);

                    // The depth buffer is only readable now: resolve a pending pivot pick and
                    // a finished lasso against it.
                    if let Some((px, py)) = st.pending_pick.take()
                        && w > 0.0
                        && h > 0.0
                    {
                        let dx = ((px * scale as f64) as i32).clamp(0, area.width() * scale - 1);
                        let dy = (((h - py) * scale as f64) as i32).clamp(0, area.height() * scale - 1);
                        let depth = renderer.read_depth(dx, dy);
                        let vp = st.camera.view_projection((w / h) as f32);
                        st.pivot = (depth < 1.0)
                            .then(|| {
                                rs_render::unproject(&vp, (2.0 * px / w - 1.0) as f32, (1.0 - 2.0 * py / h) as f32, depth)
                            })
                            .flatten();
                        let (ox, oy) = std::mem::take(&mut st.pending_orbit);
                        if ox != 0.0 || oy != 0.0 {
                            let pivot = st.pivot.unwrap_or(st.camera.target);
                            st.camera.orbit_about(pivot, ox, oy);
                            // A repaint requested from inside the render handler is dropped;
                            // ask again once it has returned.
                            let area = area.clone();
                            glib::idle_add_local_once(move || area.queue_render());
                        }
                    }
                    if let Some((px, py)) = st.pending_point_pick.take()
                        && w > 0.0
                        && h > 0.0
                    {
                        let dx = ((px * scale as f64) as i32).clamp(0, area.width() * scale - 1);
                        let dy = (((h - py) * scale as f64) as i32).clamp(0, area.height() * scale - 1);
                        let depth = renderer.read_depth(dx, dy);
                        let vp = st.camera.view_projection((w / h) as f32);
                        let hit = (depth < 1.0 && st.show_mesh)
                            .then(|| {
                                rs_render::unproject(&vp, (2.0 * px / w - 1.0) as f32, (1.0 - 2.0 * py / h) as f32, depth)
                            })
                            .flatten()
                            .and_then(|p| st.mesh.as_ref().map(|m| (p, nearest_normal(m, p))));
                        if let Some((p, n)) = hit {
                            let this = this.clone();
                            glib::idle_add_local_once(move || {
                                if let Some(cb) = this.on_point_picked.borrow().as_ref() {
                                    cb(p, n);
                                }
                            });
                        }
                    }
                    if let Some((outline, mode)) = st.lasso_pending.take() {
                        let cameras = resolve_lasso(
                            LassoInput {
                                camera: &st.camera,
                                cameras: &st.cameras,
                                mesh: if st.show_mesh { st.mesh.as_ref() } else { None },
                                selected: &mut st.selected,
                                selected_count: &mut st.selected_count,
                                dirty_selection: &mut st.dirty_selection,
                            },
                            renderer,
                            &outline,
                            mode,
                            w,
                            h,
                            scale,
                        );
                        let this = this.clone();
                        glib::idle_add_local_once(move || {
                            match cameras {
                                Some(hits) => {
                                    if let Some(cb) = this.on_cameras_lassoed.borrow().as_ref() {
                                        cb(hits, mode);
                                    }
                                }
                                None => this.selection_changed(),
                            }
                            // The highlight is uploaded on the next render; requesting it from
                            // inside the render handler would be dropped.
                            this.area.queue_render();
                        });
                    }
                }
                gtk::glib::Propagation::Stop
            }
        });

        reset.connect_clicked({
            let this = this.clone();
            move |_| this.reset_view()
        });
        this.points_toggle.connect_toggled({
            let this = this.clone();
            move |t| {
                this.state.borrow_mut().show_points = t.is_active();
                this.area.queue_render();
            }
        });
        this.mesh_toggle.connect_toggled({
            let this = this.clone();
            move |t| {
                this.state.borrow_mut().show_mesh = t.is_active();
                this.area.queue_render();
            }
        });
        this.texture_toggle.connect_toggled({
            let this = this.clone();
            move |t| {
                this.state.borrow_mut().show_textures = t.is_active();
                this.area.queue_render();
            }
        });
        cameras_toggle.connect_toggled({
            let this = this.clone();
            move |t| {
                let mut st = this.state.borrow_mut();
                st.show_cameras = t.is_active();
                st.dirty_lines = true;
                drop(st);
                this.area.queue_render();
            }
        });
        region_toggle.connect_toggled({
            let this = this.clone();
            move |t| {
                let mut st = this.state.borrow_mut();
                st.show_region = t.is_active();
                st.dirty_lines = true;
                drop(st);
                this.area.queue_render();
            }
        });

        // Orbit / pan with any button; the button decides which.
        let drag = gtk::GestureDrag::new();
        drag.set_button(0);
        drag.connect_drag_begin({
            let this = this.clone();
            move |gesture, x, y| {
                let left = gesture.current_button() == 1;
                let lasso = left && this.state.borrow().lasso_mode;
                let handle = if left && !lasso { this.pick_handle(x, y) } else { None };
                let mut st = this.state.borrow_mut();
                st.last_pointer = (x, y);
                st.drag_moved = 0.0;
                st.region_drag = handle.and_then(|h| this.begin_region_drag(&st, h, (x, y)));
                st.pivot = None;
                st.pending_orbit = (0.0, 0.0);
                st.lasso.clear();
                if lasso {
                    st.lasso.push((x, y));
                } else if left && st.region_drag.is_none() {
                    // Orbit around whatever is under the pointer; the next render looks it up.
                    st.pending_pick = Some((x, y));
                    drop(st);
                    this.area.queue_render();
                }
            }
        });
        drag.connect_drag_update({
            let this = this.clone();
            move |gesture, dx, dy| {
                let Some((sx, sy)) = gesture.start_point() else { return };
                let (x, y) = (sx + dx, sy + dy);
                let mut st = this.state.borrow_mut();
                let (lx, ly) = st.last_pointer;
                let (ddx, ddy) = ((x - lx) as f32, (y - ly) as f32);
                st.last_pointer = (x, y);
                st.drag_moved = st.drag_moved.max(dx.hypot(dy));
                if !st.lasso.is_empty() {
                    st.lasso.push((x, y));
                    drop(st);
                    this.lasso_layer.queue_draw();
                    return;
                }
                if st.region_drag.is_some() {
                    Viewport::update_region_drag(&mut st, (x, y));
                    drop(st);
                    this.area.queue_render();
                    return;
                }
                match gesture.current_button() {
                    1 => {
                        if st.pending_pick.is_some() {
                            // The pivot isn't known yet; apply once it is.
                            st.pending_orbit.0 += ddx;
                            st.pending_orbit.1 += ddy;
                        } else {
                            let pivot = st.pivot.unwrap_or(st.camera.target);
                            st.camera.orbit_about(pivot, ddx, ddy);
                        }
                    }
                    _ => {
                        let h = this.area.height() as f32;
                        st.camera.pan(ddx, ddy, h)
                    }
                }
                drop(st);
                this.area.queue_render();
            }
        });
        drag.connect_drag_end({
            let this = this.clone();
            move |gesture, _dx, _dy| {
                // A finished lasso is resolved on the next render (it needs the depth buffer).
                let lasso = {
                    let mut st = this.state.borrow_mut();
                    st.pending_pick = None;
                    st.pending_orbit = (0.0, 0.0);
                    (!st.lasso.is_empty()).then(|| (std::mem::take(&mut st.lasso), st.drag_moved))
                };
                if let Some((outline, moved)) = lasso {
                    this.lasso_layer.queue_draw();
                    let state = gesture.current_event_state();
                    let mode = if state.contains(gtk::gdk::ModifierType::SHIFT_MASK) {
                        LassoMode::Add
                    } else if state.contains(gtk::gdk::ModifierType::CONTROL_MASK) {
                        LassoMode::Subtract
                    } else {
                        LassoMode::Replace
                    };
                    if outline.len() >= 3 && moved > 4.0 {
                        this.state.borrow_mut().lasso_pending = Some((outline, mode));
                        this.area.queue_render();
                    } else if mode == LassoMode::Replace {
                        this.clear_selection();
                    }
                    return;
                }
                // A finished gizmo drag hands the new region to the application.
                let finished = {
                    let mut st = this.state.borrow_mut();
                    let drag = st.region_drag.take();
                    drag.filter(|_| st.drag_moved > 2.0).and(st.region_model.clone())
                };
                if let Some(region) = finished {
                    if let Some(cb) = this.on_region_changed.borrow().as_ref() {
                        cb(region);
                    }
                    return;
                }
                // A left button press that barely moved is a click: pick a camera.
                if gesture.current_button() != 1 {
                    return;
                }
                let Some((x, y)) = gesture.start_point() else { return };
                let moved = this.state.borrow().drag_moved;
                if moved > 4.0 {
                    return;
                }
                if gesture.current_event_state().contains(gtk::gdk::ModifierType::CONTROL_MASK) {
                    this.state.borrow_mut().pending_point_pick = Some((x, y));
                    this.area.queue_render();
                    return;
                }
                let hit = this.pick_camera(x, y);
                if let Some(cb) = this.on_camera_clicked.borrow().as_ref() {
                    cb(hit);
                }
            }
        });
        this.area.add_controller(drag);

        // Double-click on a camera opens its image.
        let click = gtk::GestureClick::new();
        click.set_button(1);
        click.connect_released({
            let this = this.clone();
            move |gesture, n_press, x, y| {
                if n_press == 2
                    && let Some(index) = this.pick_camera(x, y)
                    && let Some(cb) = this.on_camera_activated.borrow().as_ref()
                {
                    gesture.set_state(gtk::EventSequenceState::Claimed);
                    cb(index);
                }
            }
        });
        this.area.add_controller(click);

        // Hovering a gizmo handle highlights it and shows a pointer cursor.
        let motion = gtk::EventControllerMotion::new();
        motion.connect_motion({
            let this = this.clone();
            move |_, x, y| {
                if this.state.borrow().region_drag.is_some() {
                    return;
                }
                let hover = this.pick_handle(x, y);
                let mut st = this.state.borrow_mut();
                if hover != st.hover {
                    st.hover = hover;
                    rebuild_region(&mut st);
                    drop(st);
                    this.area.set_cursor_from_name(Some(if hover.is_some() { "pointer" } else { "default" }));
                    this.area.queue_render();
                }
            }
        });
        motion.connect_leave({
            let this = this.clone();
            move |_| {
                let mut st = this.state.borrow_mut();
                if st.hover.take().is_some() {
                    rebuild_region(&mut st);
                    drop(st);
                    this.area.queue_render();
                }
            }
        });
        this.area.add_controller(motion);

        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        scroll.connect_scroll({
            let this = this.clone();
            move |_, _dx, dy| {
                this.state.borrow_mut().camera.zoom(1.15f32.powf(-dy as f32));
                this.area.queue_render();
                gtk::glib::Propagation::Stop
            }
        });
        this.area.add_controller(scroll);
        this
    }

    pub fn widget(&self) -> &gtk::Overlay {
        &self.root
    }

    /// Called with the RealityScan camera index under a left click, or `None` for empty space.
    pub fn connect_camera_clicked(&self, f: impl Fn(Option<u32>) + 'static) {
        *self.on_camera_clicked.borrow_mut() = Some(Box::new(f));
    }

    /// Called with the camera index under a double-click.
    pub fn connect_camera_activated(&self, f: impl Fn(u32) + 'static) {
        *self.on_camera_activated.borrow_mut() = Some(Box::new(f));
    }

    /// Camera whose centre projects closest to the click, within [`PICK_RADIUS`].
    fn pick_camera(&self, x: f64, y: f64) -> Option<u32> {
        let st = self.state.borrow();
        let (w, h) = (self.area.width() as f64, self.area.height() as f64);
        if w <= 0.0 || h <= 0.0 {
            return None;
        }
        let vp = st.camera.view_projection((w / h) as f32);
        let mut best: Option<(f64, u32)> = None;
        for cam in &st.cameras {
            let Some((nx, ny, _)) = rs_render::project(&vp, cam.position) else { continue };
            let px = (nx as f64 + 1.0) * 0.5 * w;
            let py = (1.0 - ny as f64) * 0.5 * h;
            let d = (px - x).hypot(py - y);
            if d <= PICK_RADIUS && best.is_none_or(|(bd, _)| d < bd) {
                best = Some((d, cam.index));
            }
        }
        best.map(|(_, i)| i)
    }

    /// Highlight these cameras (RealityScan camera indices).
    pub fn set_selected_cameras(&self, indices: &[u32]) {
        let mut st = self.state.borrow_mut();
        let new: HashSet<u32> = indices.iter().copied().collect();
        if new == st.selected_cameras {
            return;
        }
        st.selected_cameras = new;
        st.dirty_lines = true;
        drop(st);
        self.area.queue_render();
    }

    /// Replace the displayed sparse cloud and cameras, and frame them.
    pub fn set_scene(&self, cloud: &SparseCloud, cameras: &[CameraPose]) {
        let positions: Vec<Vec3> = cloud.positions.iter().map(|p| Vec3::from(*p)).collect();
        let cam_positions: Vec<Vec3> = cameras
            .iter()
            .map(|c| Vec3::new(c.position[0] as f32, c.position[1] as f32, c.position[2] as f32))
            .collect();
        // Frame the cameras and the bulk of the points; sparse clouds have far outliers.
        let focus = match (rs_render::bounds(&cam_positions), rs_render::robust_bounds(&positions, 0.05)) {
            (Some((a0, a1)), Some((b0, b1))) => Some((a0.min(b0), a1.max(b1))),
            (a, b) => a.or(b),
        };
        let diag = focus.map_or(1.0, |(lo, hi)| (hi - lo).length().max(1e-3));

        let views: Vec<CameraView> = cameras
            .iter()
            .zip(&cam_positions)
            .map(|(cam, position)| {
                // RealityScan lists the rotation row by row; glam wants columns.
                let rotation = Mat3::from_cols_array(&cam.rotation.map(|v| v as f32)).transpose();
                let pose = Pose::from_normalized_focal(
                    *position,
                    rotation,
                    cam.focal as f32,
                    cam.width as f32,
                    cam.height as f32,
                );
                CameraView { index: cam.index, position: *position, pose }
            })
            .collect();
        let axes = focus.map(|(lo, hi)| rs_render::axes((lo + hi) * 0.5, diag * 0.1)).unwrap_or_default();

        let mut st = self.state.borrow_mut();
        if st.mesh.is_none()
            && let Some((lo, hi)) = focus
        {
            st.camera = OrbitCamera::framing(lo, hi, st.camera.up);
        }
        st.positions = positions;
        st.colors = cloud.colors.clone();
        st.cameras = views;
        st.frustum_depth = diag * 0.04;
        st.axes = axes;
        st.focus = focus;
        st.dirty_points = true;
        st.dirty_lines = true;
        drop(st);
        self.area.queue_render();
    }

    /// Show control points as crosses; `selected` is drawn white and larger.
    pub fn set_control_points(&self, points: &[([f32; 3], bool)]) {
        let mut st = self.state.borrow_mut();
        let diag = st.focus.map_or(1.0, |(lo, hi)| (hi - lo).length().max(1e-3));
        let mut out = Vec::with_capacity(points.len() * 3);
        for (p, selected) in points {
            let c = Vec3::from(*p);
            let h = diag * if *selected { 0.012 } else { 0.008 };
            let color = if *selected { SELECTED_CAMERA_COLOR } else { CONTROL_POINT_COLOR };
            for axis in [Vec3::X, Vec3::Y, Vec3::Z] {
                out.push(Segment { a: c - axis * h, b: c + axis * h, color });
            }
        }
        st.control_points = out;
        st.dirty_lines = true;
        drop(st);
        self.area.queue_render();
    }

    /// Called when the user finishes dragging the region gizmo.
    pub fn connect_region_changed(&self, f: impl Fn(Region) + 'static) {
        *self.on_region_changed.borrow_mut() = Some(Box::new(f));
    }

    /// Show (or hide, with `None`) the reconstruction region box. Ignored mid-drag.
    pub fn set_region(&self, region: Option<&Region>) {
        let mut st = self.state.borrow_mut();
        if st.region_drag.is_some() {
            return;
        }
        st.region_model = region.cloned();
        rebuild_region(&mut st);
        drop(st);
        self.area.queue_render();
    }

    /// Developer hook: drag a gizmo handle by `dx`/`dy` pixels as if with the mouse.
    pub fn script_region_drag(&self, handle: &str, dx: f64, dy: f64) {
        let handle = match handle {
            "centre" | "center" => Handle::Centre,
            h => {
                let (positive, axis) = match h.as_bytes() {
                    [sign, axis] => (*sign != b'-', (*axis as char).to_ascii_lowercase()),
                    _ => return,
                };
                let axis = match axis {
                    'x' => 0,
                    'y' => 1,
                    'z' => 2,
                    _ => return,
                };
                Handle::Face { axis, positive }
            }
        };
        let region = {
            let mut st = self.state.borrow_mut();
            let Some((vp, w, h)) = self.view_projection(&st) else { return };
            let Some(pos) = st.region_model.as_ref().and_then(|r| {
                region_handles(r)
                    .into_iter()
                    .find(|(hh, _)| *hh == handle)
                    .and_then(|(_, p)| to_pixels(&vp, p, w, h))
            }) else {
                return;
            };
            st.region_drag = self.begin_region_drag(&st, handle, pos);
            Viewport::update_region_drag(&mut st, (pos.0 + dx, pos.1 + dy));
            st.region_drag = None;
            st.region_model.clone()
        };
        self.area.queue_render();
        if let (Some(region), Some(cb)) = (region, self.on_region_changed.borrow().as_ref()) {
            cb(region);
        }
    }

    fn view_projection(&self, st: &State) -> Option<(Mat4, f64, f64)> {
        let (w, h) = (self.area.width() as f64, self.area.height() as f64);
        (w > 0.0 && h > 0.0).then(|| (st.camera.view_projection((w / h) as f32), w, h))
    }

    /// Gizmo handle under the pointer, within [`PICK_RADIUS`].
    fn pick_handle(&self, x: f64, y: f64) -> Option<Handle> {
        let st = self.state.borrow();
        if !st.show_region {
            return None;
        }
        let region = st.region_model.as_ref()?;
        let (vp, w, h) = self.view_projection(&st)?;
        let mut best: Option<(f64, Handle)> = None;
        for (handle, pos) in region_handles(region) {
            let Some((px, py)) = to_pixels(&vp, pos, w, h) else { continue };
            let d = (px - x).hypot(py - y);
            if d <= PICK_RADIUS && best.is_none_or(|(bd, _)| d < bd) {
                best = Some((d, handle));
            }
        }
        best.map(|(_, h)| h)
    }

    fn begin_region_drag(&self, st: &State, handle: Handle, pointer: (f64, f64)) -> Option<RegionDrag> {
        let region = st.region_model.clone()?;
        let (vp, w, h) = self.view_projection(st)?;
        let axes = region_axes(&region);
        let centre = region_centre(&region);
        let diag = (region.size[0].powi(2) + region.size[1].powi(2) + region.size[2].powi(2)).sqrt() as f32;
        let (axis, px_per_unit) = match handle {
            Handle::Face { axis, positive } => {
                let dir = axes[axis] * if positive { 1.0 } else { -1.0 };
                let p0 = centre + dir * (region.size[axis] as f32 * 0.5);
                let len = (diag * 0.1).max(1e-3);
                let a = to_pixels(&vp, p0, w, h)?;
                let b = to_pixels(&vp, p0 + dir * len, w, h)?;
                (dir, ((b.0 - a.0) / len as f64, (b.1 - a.1) / len as f64))
            }
            Handle::Centre => (Vec3::ZERO, (0.0, 0.0)),
        };
        let view = st.camera.view();
        let right = Vec3::new(view.x_axis.x, view.y_axis.x, view.z_axis.x);
        let up = Vec3::new(view.x_axis.y, view.y_axis.y, view.z_axis.y);
        let depth = -view.transform_point3(centre).z;
        let world_per_px = 2.0 * depth.max(1e-3) * (st.camera.fov_y * 0.5).tan() / h as f32;
        Some(RegionDrag {
            handle,
            start: region,
            start_pointer: pointer,
            axis,
            px_per_unit,
            world_per_px,
            right,
            up,
        })
    }

    fn update_region_drag(st: &mut State, pointer: (f64, f64)) {
        let Some(drag) = st.region_drag.as_ref() else { return };
        let (dx, dy) = (pointer.0 - drag.start_pointer.0, pointer.1 - drag.start_pointer.1);
        let mut region = drag.start.clone();
        match drag.handle {
            Handle::Face { axis, .. } => {
                let (sx, sy) = drag.px_per_unit;
                let len2 = sx * sx + sy * sy;
                if len2 < 1e-9 {
                    return;
                }
                // How far the pointer moved along the face's screen-space axis, in world units.
                let t = ((dx * sx + dy * sy) / len2) as f32;
                let new_size = (drag.start.size[axis] as f32 + t).max(0.001);
                let t = new_size - drag.start.size[axis] as f32;
                let shift = drag.axis * (t * 0.5);
                region.size[axis] = new_size as f64;
                for k in 0..3 {
                    region.centre[k] = drag.start.centre[k] + shift[k] as f64;
                }
            }
            Handle::Centre => {
                let shift = (drag.right * dx as f32 - drag.up * dy as f32) * drag.world_per_px;
                for k in 0..3 {
                    region.centre[k] = drag.start.centre[k] + shift[k] as f64;
                }
            }
        }
        st.region_model = Some(region);
        rebuild_region(st);
    }

    /// Show a mesh (hides the sparse cloud). The view is re-framed unless a mesh of about the
    /// same extent was already shown, so switching between models of one object (to compare
    /// detail levels) keeps the current zoom and angle.
    pub fn set_mesh(&self, mesh: Arc<Mesh>) {
        let textured = mesh.is_textured();
        {
            let mut st = self.state.borrow_mut();
            let previous = st.mesh.as_ref().and_then(|m| m.bounds());
            if let Some((lo, hi)) = mesh.bounds()
                && !previous.is_some_and(|old| similar_extent(old, (lo, hi)))
            {
                st.camera = OrbitCamera::framing(Vec3::from(lo), Vec3::from(hi), st.camera.up);
            }
            st.mesh = Some(mesh);
            st.dirty_mesh = true;
            st.selected.clear();
            st.selected_count = 0;
            st.dirty_selection = true;
        }
        self.selection_changed();
        self.mesh_toggle.set_sensitive(true);
        self.mesh_toggle.set_active(true);
        self.texture_toggle.set_visible(textured);
        self.points_toggle.set_active(false);
        // A newly shown model starts with everything it has turned on; hiding the texture or
        // colours of one model is not meant to carry over to the next.
        self.set_show_textures(true);
        self.set_show_colors(true);
        self.area.queue_render();
    }

    pub fn clear_mesh(&self) {
        {
            let mut st = self.state.borrow_mut();
            st.mesh = None;
            st.dirty_mesh = true;
            st.selected.clear();
            st.selected_count = 0;
            st.dirty_selection = true;
        }
        self.selection_changed();
        self.mesh_toggle.set_sensitive(false);
        self.texture_toggle.set_visible(false);
        self.points_toggle.set_active(true);
        self.area.queue_render();
    }

    pub fn clear(&self) {
        {
            let mut st = self.state.borrow_mut();
            st.positions.clear();
            st.colors.clear();
            st.cameras.clear();
            st.selected_cameras.clear();
            st.axes.clear();
            st.region.clear();
            st.region_model = None;
            st.region_drag = None;
            st.control_points.clear();
            st.focus = None;
            st.mesh = None;
            st.dirty_points = true;
            st.dirty_lines = true;
            st.dirty_mesh = true;
        }
        self.mesh_toggle.set_sensitive(false);
        self.texture_toggle.set_visible(false);
        self.points_toggle.set_active(true);
        self.area.queue_render();
    }

    /// Show the texture (where the mesh has one) or the vertex colours instead.
    pub fn set_show_textures(&self, on: bool) {
        self.texture_toggle.set_active(on);
    }

    pub fn show_textures(&self) -> bool {
        self.state.borrow().show_textures
    }

    /// Show vertex colours, or a plain shaded surface.
    pub fn set_show_colors(&self, on: bool) {
        {
            let mut st = self.state.borrow_mut();
            if st.show_colors == on {
                return;
            }
            st.show_colors = on;
        }
        self.area.queue_render();
    }

    pub fn show_colors(&self) -> bool {
        self.state.borrow().show_colors
    }

    /// Ctrl+click on the mesh: the surface point and its normal.
    pub fn connect_point_picked(&self, f: impl Fn(Vec3, Vec3) + 'static) {
        *self.on_point_picked.borrow_mut() = Some(Box::new(f));
    }

    /// Scripted Ctrl+click at a fraction of the viewport.
    pub fn script_point_pick(&self, fx: f64, fy: f64) {
        let (w, h) = (self.area.width() as f64, self.area.height() as f64);
        self.state.borrow_mut().pending_point_pick = Some((fx * w, fy * h));
        self.area.queue_render();
    }

    pub fn connect_selection_changed(&self, f: impl Fn(usize) + 'static) {
        *self.on_selection_changed.borrow_mut() = Some(Box::new(f));
    }

    /// Cameras caught by a lasso while the sparse view is shown (RealityScan indices).
    pub fn connect_cameras_lassoed(&self, f: impl Fn(Vec<u32>, LassoMode) + 'static) {
        *self.on_cameras_lassoed.borrow_mut() = Some(Box::new(f));
    }

    /// "Remove" / "Keep only": the mesh and which triangles the new model keeps.
    pub fn connect_edit(&self, f: impl Fn(Arc<Mesh>, Vec<bool>) + 'static) {
        *self.on_edit.borrow_mut() = Some(Box::new(f));
    }

    /// Flip one of the toolbar's display toggles by name (`points`, `mesh`, `texture`,
    /// `cameras`, `region`, `lasso`); false if there is no such toggle. For scripted runs.
    pub fn set_display(&self, what: &str, on: bool) -> bool {
        let toggle = match what {
            "points" => &self.points_toggle,
            "mesh" => &self.mesh_toggle,
            "texture" => &self.texture_toggle,
            "cameras" => &self.cameras_toggle,
            "region" => &self.region_toggle,
            "lasso" => &self.lasso_toggle,
            _ => return false,
        };
        toggle.set_active(on);
        true
    }

    pub fn set_lasso_mode(&self, on: bool) {
        self.lasso_toggle.set_active(on);
    }

    pub fn clear_selection(&self) {
        {
            let mut st = self.state.borrow_mut();
            if st.selected_count == 0 && st.selected.iter().all(|s| !s) {
                return;
            }
            st.selected.fill(false);
            st.selected_count = 0;
            st.dirty_selection = true;
        }
        self.selection_changed();
        self.area.queue_render();
    }

    pub fn invert_selection(&self) {
        {
            let mut st = self.state.borrow_mut();
            let n = st.mesh.as_ref().map_or(0, |m| m.triangle_count());
            st.selected.resize(n, false);
            for s in &mut st.selected {
                *s = !*s;
            }
            st.selected_count = st.selected.iter().filter(|s| **s).count();
            st.dirty_selection = true;
        }
        self.selection_changed();
        self.area.queue_render();
    }

    pub fn lasso_mode(&self) -> bool {
        self.lasso_toggle.is_active()
    }

    /// Hand the current selection to the application as an edit ("Remove" / "Keep only").
    pub fn edit_selection(&self, keep_selected: bool) {
        let job = {
            let st = self.state.borrow();
            match (&st.mesh, st.selected_count) {
                (Some(mesh), n) if n > 0 => {
                    let keep: Vec<bool> = st.selected.iter().map(|s| *s == keep_selected).collect();
                    Some((Arc::clone(mesh), keep))
                }
                _ => None,
            }
        };
        if let Some((mesh, keep)) = job
            && let Some(cb) = self.on_edit.borrow().as_ref()
        {
            cb(mesh, keep);
        }
    }

    fn selection_changed(&self) {
        let (count, total) = {
            let st = self.state.borrow();
            (st.selected_count, st.mesh.as_ref().map_or(0, |m| m.triangle_count()))
        };
        self.selection_bar.set_visible(count > 0);
        self.selection_label.set_text(&format!("{count} of {total} triangles selected"));
        if let Some(cb) = self.on_selection_changed.borrow().as_ref() {
            cb(count);
        }
    }

    /// Scripted lasso: `points` in logical pixels, or fractions of the viewport when every
    /// coordinate is within 0..=1.
    pub fn script_lasso(&self, mut points: Vec<(f64, f64)>, mode: LassoMode) {
        if points.iter().all(|(x, y)| (0.0..=1.0).contains(x) && (0.0..=1.0).contains(y)) {
            let (w, h) = (self.area.width() as f64, self.area.height() as f64);
            for p in &mut points {
                *p = (p.0 * w, p.1 * h);
            }
        }
        self.state.borrow_mut().lasso_pending = Some((points, mode));
        self.area.queue_render();
    }

    /// Orbit by `dx`/`dy` pixels and zoom by `factor` (for scripted views).
    pub fn nudge(&self, dx: f32, dy: f32, factor: f32) {
        let mut st = self.state.borrow_mut();
        st.camera.orbit(dx, dy);
        st.camera.zoom(factor);
        drop(st);
        self.area.queue_render();
    }

    /// Re-frame whatever is shown: the mesh if there is one, otherwise the sparse scene.
    pub fn reset_view(&self) {
        let mut st = self.state.borrow_mut();
        let bounds = match (&st.mesh, st.show_mesh) {
            (Some(m), true) => m.bounds().map(|(lo, hi)| (Vec3::from(lo), Vec3::from(hi))),
            _ => st.focus,
        };
        if let Some((lo, hi)) = bounds {
            st.camera = OrbitCamera::framing(lo, hi, st.camera.up);
        }
        drop(st);
        self.area.queue_render();
    }
}

/// Resolve GL entry points through libepoxy, which GTK itself uses.
///
/// libepoxy exports one dispatch pointer variable per GL function, named
/// `epoxy_<function>`; it initially holds a resolver stub that is callable too.
fn load_gl() -> glow::Context {
    use std::sync::OnceLock;
    static LIB: OnceLock<libloading::os::unix::Library> = OnceLock::new();
    let lib = LIB.get_or_init(|| {
        // SAFETY: loading the system libepoxy, kept alive for the process lifetime.
        unsafe { libloading::os::unix::Library::new("libepoxy.so.0") }.expect("libepoxy.so.0")
    });
    // SAFETY: the GTK GL context is current (called from realize); each variable holds
    // a function pointer with the GL signature of the same name, by libepoxy's contract.
    unsafe {
        glow::Context::from_loader_function(|name| {
            let symbol = format!("epoxy_{name}\0");
            // `Symbol<T>` dereferences to the symbol's address; the variable's value is one
            // more dereference away.
            lib.get::<*const *const std::ffi::c_void>(symbol.as_bytes())
                .map(|var| **var)
                .unwrap_or(std::ptr::null())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn similar_extent_means_same_object() {
        let a = ([0.0, 0.0, 0.0], [2.0, 2.0, 2.0]);
        assert!(
            similar_extent(a, ([0.1, -0.1, 0.0], [2.2, 1.9, 2.1])),
            "a slightly different mesh of the same thing"
        );
        assert!(similar_extent(a, ([0.5, 0.5, 0.5], [1.5, 1.5, 1.5])), "a cropped version");
        assert!(!similar_extent(a, ([10.0, 0.0, 0.0], [12.0, 2.0, 2.0])), "somewhere else");
        assert!(!similar_extent(a, ([0.0, 0.0, 0.0], [20.0, 20.0, 20.0])), "ten times bigger");
    }

    #[test]
    fn polygon_test_is_even_odd() {
        let square = [(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)];
        assert!(inside_polygon(&square, 5.0, 5.0));
        assert!(!inside_polygon(&square, 15.0, 5.0));
        let concave = [(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (5.0, 2.0), (0.0, 10.0)];
        assert!(!inside_polygon(&concave, 5.0, 6.0), "inside the notch");
        assert!(inside_polygon(&concave, 1.0, 1.0));
    }

    #[test]
    fn nearest_normal_picks_the_closest_vertex() {
        let mesh = Mesh {
            positions: vec![[0.0, 0.0, 0.0], [5.0, 0.0, 0.0]],
            normals: vec![[1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            ..Mesh::default()
        };
        assert_eq!(nearest_normal(&mesh, Vec3::new(4.0, 0.2, 0.0)), Vec3::Y);
        assert_eq!(nearest_normal(&Mesh::default(), Vec3::ZERO), Vec3::Z);
    }
}
