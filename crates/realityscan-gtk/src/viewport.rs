//! 3D viewport: a `GtkGLArea` driven by `rs_render`, with an overlay toolbar.
//!
//! Mouse: left-drag orbits, middle- or right-drag pans, wheel zooms, a left click
//! on a camera selects it (reported through [`Viewport::connect_camera_clicked`]).

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;

use gtk4 as gtk;

use glam::{Mat3, Mat4, Vec3};
use gtk::glib;
use gtk::prelude::*;
use rs_render::{MeshPart, OrbitCamera, Pose, Renderer, Segment, TextureUpload, ViewPreset};
use rs_scene::mesh::Mesh;
use rs_scene::region::Region;
use rs_scene::sparse::{CameraPose, SparseCloud};

/// Side of the navigation cube widget, in logical pixels.
const NAV_CUBE_SIZE: i32 = 140;
/// Settings key remembering the parallel-projection toggle.
const ORTHO_KEY: &str = "view-orthographic";
/// Click tolerance for picking a camera, in logical pixels.
const PICK_RADIUS: f64 = 14.0;
const CAMERA_COLOR: [u8; 3] = [255, 200, 60];
const SELECTED_CAMERA_COLOR: [u8; 3] = [255, 255, 255];
const REGION_COLOR: [u8; 3] = [80, 200, 255];
/// The region's bottom face (local −Z): the face "Set ground from region" puts on the ground.
const GROUND_FACE_COLOR: [u8; 3] = [255, 165, 40];
const GROUND_GRID_COLOR: [u8; 3] = [150, 100, 35];
/// Grid cells along each side of the ground face.
const GROUND_GRID_CELLS: usize = 4;
const CONTROL_POINT_COLOR: [u8; 3] = [255, 90, 220];
const HOVER_COLOR: [u8; 3] = [255, 255, 255];
/// How far the face handles stand off their faces, as a fraction of the box diagonal, so
/// they stay visible and clear of the box edges when looking straight at a face.
const HANDLE_STICK_OUT: f32 = 0.12;
/// Handle discs (drawn in screen space), in logical pixels; hovered ones grow.
const HANDLE_DISC_RADIUS: f64 = 5.5;
const HANDLE_HOVER_RADIUS: f64 = 8.5;
/// Face handles whose stem is shorter than this on screen point at the viewer: they can't be
/// dragged meaningfully, so they are neither drawn nor picked.
const HANDLE_MIN_STEM_PX: f64 = 8.0;
/// Pick tolerance for the rotation rings, in logical pixels (kept tight so an orbit that
/// starts near a ring doesn't turn the box by accident).
const RING_PICK_RADIUS: f64 = 7.0;
/// Length of the coloured local-axis markers at the box's (−X, −Y, −Z) corner, as a fraction
/// of each edge.
const AXIS_MARKER_FRACTION: f32 = 0.3;

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
    /// Ring around the box's local `axis`; dragging it turns the box about that axis
    /// (Ctrl snaps to 15°).
    Ring { axis: usize },
}

/// Ring colours per local axis (X red, Y green, Z blue), as gizmos usually are.
const RING_COLORS: [[u8; 3]; 3] = [[235, 90, 90], [110, 210, 110], [100, 150, 255]];
const RING_SEGMENTS: usize = 64;

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
    /// Ring drags: the box centre on screen, the pointer's start angle around it, the
    /// world rotation axis and whether it points towards the viewer.
    centre_px: (f64, f64),
    start_angle: f64,
    towards_viewer: bool,
    /// Snap rotations to 15° (Ctrl held).
    snap: bool,
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
    ortho_toggle: gtk::ToggleButton,
    /// Navigation cube overlay and the face under the pointer.
    cube: gtk::DrawingArea,
    cube_hover: Rc<Cell<Option<ViewPreset>>>,
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
            // Off by default: with its rings the box crowds the scene. Opening the Region
            // panel shows it.
            show_region: false,
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

fn region_diagonal(r: &Region) -> f32 {
    (r.size[0] * r.size[0] + r.size[1] * r.size[1] + r.size[2] * r.size[2]).sqrt() as f32
}

/// Handles with the world point they are attached to and where they are drawn and grabbed:
/// a face handle sits on a stem standing out of its face centre; the centre handle is the
/// box centre.
fn region_handles(r: &Region) -> Vec<(Handle, Vec3, Vec3)> {
    let axes = region_axes(r);
    let c = region_centre(r);
    let stick = (region_diagonal(r) * HANDLE_STICK_OUT).max(1e-3);
    let mut out = Vec::with_capacity(7);
    for (axis, dir) in axes.iter().enumerate() {
        for positive in [true, false] {
            let sign = if positive { 1.0 } else { -1.0 };
            let base = c + *dir * (r.size[axis] as f32 * 0.5 * sign);
            out.push((Handle::Face { axis, positive }, base, base + *dir * (stick * sign)));
        }
    }
    out.push((Handle::Centre, c, c));
    out
}

/// A point on screen, in logical pixels.
type Px = (f64, f64);

/// Screen positions of the handles that can be grabbed from this view: (handle, base, tip).
fn visible_handles(r: &Region, vp: &Mat4, w: f64, h: f64) -> Vec<(Handle, Px, Px)> {
    region_handles(r)
        .into_iter()
        .filter_map(|(handle, base, tip)| {
            let (b, t) = (to_pixels(vp, base, w, h)?, to_pixels(vp, tip, w, h)?);
            let edge_on =
                matches!(handle, Handle::Face { .. }) && (t.0 - b.0).hypot(t.1 - b.1) < HANDLE_MIN_STEM_PX;
            (!edge_on).then_some((handle, b, t))
        })
        .collect()
}

/// Radius of the rotation rings: just outside the box's circumscribed sphere.
fn ring_radius(r: &Region) -> f32 {
    (region_diagonal(r) * 0.5 * 1.08).max(1e-3)
}

/// Points around the ring about local `axis` (closed: the first point is repeated).
fn ring_points(r: &Region, axis: usize) -> Vec<Vec3> {
    let axes = region_axes(r);
    let (u, v) = (axes[(axis + 1) % 3], axes[(axis + 2) % 3]);
    let (c, rad) = (region_centre(r), ring_radius(r));
    (0..=RING_SEGMENTS)
        .map(|i| {
            let a = i as f32 / RING_SEGMENTS as f32 * std::f32::consts::TAU;
            c + (u * a.cos() + v * a.sin()) * rad
        })
        .collect()
}

/// Rotation matrix (row-major) for `angle` radians about the unit vector `n`.
fn axis_angle_matrix(n: Vec3, angle: f64) -> [[f64; 3]; 3] {
    let m = Mat3::from_axis_angle(n.normalize(), angle as f32);
    [0, 1, 2].map(|i| [0, 1, 2].map(|j| m.col(j)[i] as f64))
}

fn mat_mul(a: [[f64; 3]; 3], b: [[f64; 3]; 3]) -> [[f64; 3]; 3] {
    let mut out = [[0.0; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            out[i][j] = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    out
}

/// Edges of the region box plus its handles (a small square per face, a cross at the centre).
fn region_segments(r: &Region, hover: Option<Handle>, with_handles: bool) -> Vec<Segment> {
    let corners = r.corners().map(|c| Vec3::new(c[0] as f32, c[1] as f32, c[2] as f32));
    // Corners 0–3 have local z = −½: the bottom face. Its edges get their own colour and a grid
    // fills it, so it reads as the floor the ground plane will be set on.
    let mut out: Vec<Segment> = Vec::with_capacity(64 + 3 * RING_SEGMENTS);
    for &(a, b) in &Region::EDGES {
        let color = if a < 4 && b < 4 { GROUND_FACE_COLOR } else { REGION_COLOR };
        if a == 0 {
            // The three edges leaving corner 0 (−X, −Y, −Z) start in their axis colour, so
            // the box shows which way its local X, Y and Z run.
            let axis = b.trailing_zeros() as usize;
            let mid = corners[0].lerp(corners[b], AXIS_MARKER_FRACTION);
            out.push(Segment { a: corners[0], b: mid, color: RING_COLORS[axis] });
            out.push(Segment { a: mid, b: corners[b], color });
        } else {
            out.push(Segment { a: corners[a], b: corners[b], color });
        }
    }
    for i in 1..GROUND_GRID_CELLS {
        let t = i as f32 / GROUND_GRID_CELLS as f32;
        // Lines parallel to local X (between the −Y and +Y edges) and to local Y.
        out.push(Segment {
            a: corners[0].lerp(corners[2], t),
            b: corners[1].lerp(corners[3], t),
            color: GROUND_GRID_COLOR,
        });
        out.push(Segment {
            a: corners[0].lerp(corners[1], t),
            b: corners[2].lerp(corners[3], t),
            color: GROUND_GRID_COLOR,
        });
    }
    if !with_handles {
        return out;
    }
    // The face and centre handles are drawn in screen space on the overlay layer (see
    // `draw_gizmo_overlay`); the rings are drawn here, with a thick overlay copy when hovered.
    for (axis, ring_color) in RING_COLORS.into_iter().enumerate() {
        let color = if hover == Some(Handle::Ring { axis }) { HOVER_COLOR } else { ring_color };
        let pts = ring_points(r, axis);
        out.extend(pts.windows(2).map(|w| Segment { a: w[0], b: w[1], color }));
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

/// Faces of the navigation cube: the view each selects, with its label.
const NAV_CUBE_FACES: [(ViewPreset, &str); 6] = [
    (ViewPreset::Front, "Front"),
    (ViewPreset::Back, "Back"),
    (ViewPreset::Left, "Left"),
    (ViewPreset::Right, "Right"),
    (ViewPreset::Top, "Top"),
    (ViewPreset::Bottom, "Bottom"),
];

/// A navigation-cube face as seen now: its view, label, screen corners, text axes (right,
/// down) in pixels per unit face width, and how directly it faces the viewer (0–1).
struct CubeFace {
    preset: ViewPreset,
    label: &'static str,
    quad: [(f64, f64); 4],
    centre: (f64, f64),
    right: (f64, f64),
    down: (f64, f64),
    facing: f64,
}

/// The cube faces that face the viewer, farthest first (paint order).
fn nav_cube_faces(camera: &OrbitCamera, w: f64, h: f64) -> Vec<CubeFace> {
    let view = camera.view();
    let scale = w.min(h) * 0.36;
    let (cx, cy) = (w * 0.5, h * 0.5);
    // Rotation only: the cube sits at the widget centre whatever the camera position.
    let project = |v: Vec3| {
        let p = view.transform_vector3(v);
        (p.x as f64 * scale, -p.y as f64 * scale, p.z as f64)
    };
    let snap = |v: Vec3| {
        // Preset cameras tilt a hair off the poles; round their axes to the world axes.
        let a = v.abs();
        if a.x >= a.y && a.x >= a.z {
            Vec3::X * v.x.signum()
        } else if a.y >= a.z {
            Vec3::Y * v.y.signum()
        } else {
            Vec3::Z * v.z.signum()
        }
    };
    let mut faces: Vec<(f64, CubeFace)> = NAV_CUBE_FACES
        .iter()
        .filter_map(|&(preset, label)| {
            // The face a preset looks at, with that view's right and up as the text axes.
            let mut c = camera.clone();
            c.set_preset(preset);
            let v = c.view();
            let normal = snap(c.eye() - c.target);
            let right = snap(Vec3::new(v.x_axis.x, v.y_axis.x, v.z_axis.x));
            let up = snap(Vec3::new(v.x_axis.y, v.y_axis.y, v.z_axis.y));
            let facing = project(normal).2;
            if facing <= 0.02 {
                return None;
            }
            let centre3 = normal * 0.5;
            let corner = |a: f32, b: f32| {
                let p = project(centre3 + right * (0.5 * a) + up * (0.5 * b));
                (cx + p.0, cy + p.1)
            };
            let c0 = project(centre3);
            let r = project(right);
            let d = project(-up);
            Some((
                c0.2,
                CubeFace {
                    preset,
                    label,
                    quad: [corner(-1.0, 1.0), corner(1.0, 1.0), corner(1.0, -1.0), corner(-1.0, -1.0)],
                    centre: (cx + c0.0, cy + c0.1),
                    right: (r.0, r.1),
                    down: (d.0, d.1),
                    facing,
                },
            ))
        })
        .collect();
    faces.sort_by(|a, b| a.0.total_cmp(&b.0));
    faces.into_iter().map(|(_, f)| f).collect()
}

/// The face under (`x`, `y`), nearest the viewer first.
fn nav_cube_pick(camera: &OrbitCamera, w: f64, h: f64, x: f64, y: f64) -> Option<ViewPreset> {
    nav_cube_faces(camera, w, h).into_iter().rev().find(|f| inside_polygon(&f.quad, x, y)).map(|f| f.preset)
}

fn draw_nav_cube(
    area: &gtk::DrawingArea,
    cr: &gtk::cairo::Context,
    w: f64,
    h: f64,
    camera: &OrbitCamera,
    hover: Option<ViewPreset>,
) {
    let fg = area.color();
    let (fr, fg_, fb) = (fg.red() as f64, fg.green() as f64, fg.blue() as f64);
    cr.set_line_join(gtk::cairo::LineJoin::Round);
    cr.select_font_face("Sans", gtk::cairo::FontSlant::Normal, gtk::cairo::FontWeight::Bold);
    for face in nav_cube_faces(camera, w, h) {
        let hot = hover == Some(face.preset);
        cr.move_to(face.quad[0].0, face.quad[0].1);
        for p in &face.quad[1..] {
            cr.line_to(p.0, p.1);
        }
        cr.close_path();
        // Shade by how directly the face looks at the viewer, so the cube reads as solid.
        let shade = 0.22 + 0.18 * face.facing;
        if hot {
            cr.set_source_rgba(0.21, 0.52, 0.89, 0.95);
        } else {
            cr.set_source_rgba(shade, shade, shade + 0.02, 0.92);
        }
        let _ = cr.fill_preserve();
        cr.set_source_rgba(fr, fg_, fb, 0.55);
        cr.set_line_width(1.0);
        let _ = cr.stroke();
        if face.facing < 0.25 {
            continue;
        }
        // Text in the face's own plane: map a unit square to the projected face.
        let _ = cr.save();
        let m = gtk::cairo::Matrix::new(
            face.right.0,
            face.right.1,
            face.down.0,
            face.down.1,
            face.centre.0,
            face.centre.1,
        );
        cr.transform(m);
        cr.set_font_size(0.24);
        if let Ok(ext) = cr.text_extents(face.label) {
            let size = if ext.width() > 0.86 { 0.24 * 0.86 / ext.width() } else { 0.24 };
            cr.set_font_size(size);
            if let Ok(ext) = cr.text_extents(face.label) {
                cr.move_to(-ext.width() / 2.0 - ext.x_bearing(), -ext.height() / 2.0 - ext.y_bearing());
                cr.set_source_rgba(1.0, 1.0, 1.0, 0.35 + 0.65 * face.facing);
                let _ = cr.show_text(face.label);
            }
        }
        let _ = cr.restore();
    }
}

/// Toolbar icon for the projection: converging edges for perspective, parallel ones for the
/// orthographic view.
fn draw_projection_icon(area: &gtk::DrawingArea, cr: &gtk::cairo::Context, w: f64, h: f64, ortho: bool) {
    let fg = area.color();
    cr.set_source_rgba(fg.red() as f64, fg.green() as f64, fg.blue() as f64, fg.alpha() as f64);
    cr.set_line_width(1.5);
    cr.set_line_join(gtk::cairo::LineJoin::Round);
    let (x0, x1, y0, y1) = (1.5, w - 1.5, 2.5, h - 2.5);
    if ortho {
        // A cube drawn with parallel edges.
        let d = (w * 0.3).round();
        cr.rectangle(x0, y0 + d, x1 - x0 - d, y1 - y0 - d);
        cr.move_to(x0, y0 + d);
        cr.line_to(x0 + d, y0);
        cr.line_to(x1, y0);
        cr.line_to(x1 - d, y0 + d);
        cr.move_to(x1, y0);
        cr.line_to(x1, y1 - d);
        cr.line_to(x1 - d, y1);
    } else {
        // A frustum: the far side is smaller, the edges converge.
        let (mx, my) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
        let (fw, fh) = ((x1 - x0) * 0.22, (y1 - y0) * 0.22);
        cr.rectangle(x0, y0, x1 - x0, y1 - y0);
        cr.rectangle(mx - fw, my - fh, fw * 2.0, fh * 2.0);
        for (ax, ay, bx, by) in [
            (x0, y0, mx - fw, my - fh),
            (x1, y0, mx + fw, my - fh),
            (x1, y1, mx + fw, my + fh),
            (x0, y1, mx - fw, my + fh),
        ] {
            cr.move_to(ax, ay);
            cr.line_to(bx, by);
        }
    }
    let _ = cr.stroke();
}

/// Handle names used by the script hooks: `centre`, `ring-x|y|z`, or a sign and axis (`+x`, `-z`).
fn parse_handle(name: &str) -> Option<Handle> {
    Some(match name {
        "centre" | "center" => Handle::Centre,
        "ring-x" => Handle::Ring { axis: 0 },
        "ring-y" => Handle::Ring { axis: 1 },
        "ring-z" => Handle::Ring { axis: 2 },
        h => {
            let (positive, axis) = match h.as_bytes() {
                [sign, axis] => (*sign != b'-', (*axis as char).to_ascii_lowercase()),
                _ => return None,
            };
            let axis = match axis {
                'x' => 0,
                'y' => 1,
                'z' => 2,
                _ => return None,
            };
            Handle::Face { axis, positive }
        }
    })
}

/// Distance from `p` to the segment `a`–`b` (pixels).
fn point_segment_distance(p: (f64, f64), a: (f64, f64), b: (f64, f64)) -> f64 {
    let (vx, vy) = (b.0 - a.0, b.1 - a.1);
    let len2 = vx * vx + vy * vy;
    let t = if len2 > 0.0 { (((p.0 - a.0) * vx + (p.1 - a.1) * vy) / len2).clamp(0.0, 1.0) } else { 0.0 };
    (p.0 - a.0 - t * vx).hypot(p.1 - a.1 - t * vy)
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
        let region_toggle = gtk::ToggleButton::builder().label("Region").active(false).build();
        region_toggle.set_tooltip_text(Some("Show the reconstruction region"));
        let lasso_toggle = gtk::ToggleButton::builder().icon_name("edit-select-all-symbolic").build();
        lasso_toggle.set_tooltip_text(Some(
            "Lasso: drag around triangles (mesh) or cameras (points) to select them.\nShift adds, Ctrl removes, a click clears.",
        ));
        // Perspective / parallel switch; its icon shows the current projection. The choice is
        // remembered. (The axis views are on the navigation cube.)
        let ortho = crate::settings::get(ORTHO_KEY).is_some_and(|v| v == "1");
        let ortho_icon = gtk::DrawingArea::builder().content_width(16).content_height(16).build();
        let ortho_toggle = gtk::ToggleButton::builder().child(&ortho_icon).active(ortho).build();
        ortho_icon.set_draw_func({
            let toggle = ortho_toggle.clone();
            move |area, cr, w, h| draw_projection_icon(area, cr, w as f64, h as f64, toggle.is_active())
        });
        let set_ortho_tooltip = |t: &gtk::ToggleButton| {
            t.set_tooltip_text(Some(if t.is_active() {
                "Parallel (orthographic) projection — click for perspective (numpad 5)"
            } else {
                "Perspective projection — click for parallel/orthographic (numpad 5)"
            }));
        };
        set_ortho_tooltip(&ortho_toggle);
        bar.append(&reset);
        bar.append(&ortho_toggle);
        bar.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        for t in [&points_toggle, &mesh_toggle, &texture_toggle, &cameras_toggle, &region_toggle] {
            bar.append(t);
        }
        bar.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        bar.append(&lasso_toggle);

        // The lasso outline and the region handles are drawn on a transparent layer that lets
        // events through (under the toolbars).
        let lasso_layer = gtk::DrawingArea::builder().can_target(false).hexpand(true).vexpand(true).build();
        root.add_overlay(&lasso_layer);
        root.add_overlay(&bar);

        // Navigation cube in the bottom-right corner: it turns with the view, and clicking a
        // face looks at the scene from that side.
        let cube = gtk::DrawingArea::builder()
            .content_width(NAV_CUBE_SIZE)
            .content_height(NAV_CUBE_SIZE)
            .halign(gtk::Align::End)
            .valign(gtk::Align::End)
            .margin_end(12)
            .margin_bottom(12)
            .build();
        cube.set_cursor_from_name(Some("pointer"));
        root.add_overlay(&cube);

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
        state.borrow_mut().camera.orthographic = ortho;
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
            ortho_toggle: ortho_toggle.clone(),
            cube,
            cube_hover: Rc::new(Cell::new(None)),
        };

        ortho_toggle.connect_toggled({
            let this = this.clone();
            let icon = ortho_icon.clone();
            move |t| {
                this.state.borrow_mut().camera.orthographic = t.is_active();
                crate::settings::set(ORTHO_KEY, if t.is_active() { "1" } else { "0" });
                set_ortho_tooltip(t);
                icon.queue_draw();
                this.redraw();
            }
        });
        this.cube.set_draw_func({
            let this = this.clone();
            move |area, cr, w, h| {
                let camera = this.state.borrow().camera.clone();
                draw_nav_cube(area, cr, w as f64, h as f64, &camera, this.cube_hover.get());
            }
        });
        let cube_click = gtk::GestureClick::new();
        cube_click.set_button(1);
        cube_click.connect_released({
            let this = this.clone();
            move |_, _, x, y| {
                let (w, h) = (this.cube.width() as f64, this.cube.height() as f64);
                let camera = this.state.borrow().camera.clone();
                if let Some(preset) = nav_cube_pick(&camera, w, h, x, y) {
                    this.set_view_preset(preset);
                }
            }
        });
        this.cube.add_controller(cube_click);
        let cube_motion = gtk::EventControllerMotion::new();
        cube_motion.connect_motion({
            let this = this.clone();
            move |_, x, y| {
                let (w, h) = (this.cube.width() as f64, this.cube.height() as f64);
                let camera = this.state.borrow().camera.clone();
                let hit = nav_cube_pick(&camera, w, h, x, y);
                if hit != this.cube_hover.get() {
                    this.cube_hover.set(hit);
                    this.cube.queue_draw();
                }
            }
        });
        cube_motion.connect_leave({
            let this = this.clone();
            move |_| {
                this.cube_hover.set(None);
                this.cube.queue_draw();
            }
        });
        this.cube.add_controller(cube_motion);
        // Numpad views (Blender's layout) while the 3D view has the keyboard focus.
        this.area.set_focusable(true);
        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed({
            let this = this.clone();
            move |_, key, _, mods| {
                use gtk::gdk::Key;
                let ctrl = mods.contains(gtk::gdk::ModifierType::CONTROL_MASK);
                let preset = match key {
                    Key::KP_7 | Key::KP_Home => Some(if ctrl { ViewPreset::Bottom } else { ViewPreset::Top }),
                    Key::KP_1 | Key::KP_End => Some(if ctrl { ViewPreset::Back } else { ViewPreset::Front }),
                    Key::KP_3 | Key::KP_Next => Some(if ctrl { ViewPreset::Left } else { ViewPreset::Right }),
                    Key::KP_5 | Key::KP_Begin => {
                        this.toggle_orthographic();
                        return gtk::glib::Propagation::Stop;
                    }
                    _ => None,
                };
                match preset {
                    Some(p) => {
                        this.set_view_preset(p);
                        gtk::glib::Propagation::Stop
                    }
                    None => gtk::glib::Propagation::Proceed,
                }
            }
        });
        this.area.add_controller(keys);

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
                this.draw_gizmo_overlay(cr);
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
                            let this = this.clone();
                            glib::idle_add_local_once(move || this.redraw());
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
                            this.redraw();
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
                this.redraw();
            }
        });
        this.mesh_toggle.connect_toggled({
            let this = this.clone();
            move |t| {
                this.state.borrow_mut().show_mesh = t.is_active();
                this.redraw();
            }
        });
        this.texture_toggle.connect_toggled({
            let this = this.clone();
            move |t| {
                this.state.borrow_mut().show_textures = t.is_active();
                this.redraw();
            }
        });
        cameras_toggle.connect_toggled({
            let this = this.clone();
            move |t| {
                let mut st = this.state.borrow_mut();
                st.show_cameras = t.is_active();
                st.dirty_lines = true;
                drop(st);
                this.redraw();
            }
        });
        region_toggle.connect_toggled({
            let this = this.clone();
            move |t| {
                let mut st = this.state.borrow_mut();
                st.show_region = t.is_active();
                st.dirty_lines = true;
                drop(st);
                this.redraw();
            }
        });

        // Orbit / pan with any button; the button decides which.
        let drag = gtk::GestureDrag::new();
        drag.set_button(0);
        drag.connect_drag_begin({
            let this = this.clone();
            move |gesture, x, y| {
                this.area.grab_focus();
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
                    this.redraw();
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
                if let Some(drag) = st.region_drag.as_mut() {
                    drag.snap = gesture.current_event_state().contains(gtk::gdk::ModifierType::CONTROL_MASK);
                    Viewport::update_region_drag(&mut st, (x, y));
                    drop(st);
                    this.redraw();
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
                this.redraw();
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
                        this.redraw();
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
                    this.redraw();
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
                    this.redraw();
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
                    this.redraw();
                }
            }
        });
        this.area.add_controller(motion);

        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        scroll.connect_scroll({
            let this = this.clone();
            move |_, _dx, dy| {
                this.state.borrow_mut().camera.zoom(1.15f32.powf(-dy as f32));
                this.redraw();
                gtk::glib::Propagation::Stop
            }
        });
        this.area.add_controller(scroll);
        this
    }

    /// Repaint the 3D view and the overlay drawn over it.
    fn redraw(&self) {
        self.area.queue_render();
        self.lasso_layer.queue_draw();
        self.cube.queue_draw();
    }

    /// Region handles in screen space, so they keep a constant size, stay visible through
    /// the model and can be told apart: a disc on a stem per face (in its axis colour), a
    /// square at the centre, and a thick copy of a hovered ring.
    fn draw_gizmo_overlay(&self, cr: &gtk::cairo::Context) {
        let st = self.state.borrow();
        if !st.show_region {
            return;
        }
        let Some(region) = st.region_model.as_ref() else { return };
        let Some((vp, w, h)) = self.view_projection(&st) else { return };
        let rgb = |c: [u8; 3]| (c[0] as f64 / 255.0, c[1] as f64 / 255.0, c[2] as f64 / 255.0);
        let active = st.region_drag.as_ref().map(|d| d.handle).or(st.hover);
        cr.set_line_cap(gtk::cairo::LineCap::Round);
        cr.set_line_join(gtk::cairo::LineJoin::Round);

        if let Some(Handle::Ring { axis }) = active {
            let pts: Vec<_> =
                ring_points(region, axis).into_iter().map(|p| to_pixels(&vp, p, w, h)).collect();
            let mut pen_down = false;
            for p in pts {
                match (p, pen_down) {
                    (Some((x, y)), true) => cr.line_to(x, y),
                    (Some((x, y)), false) => cr.move_to(x, y),
                    (None, _) => {}
                }
                pen_down = p.is_some();
            }
            let (r, g, b) = rgb(RING_COLORS[axis]);
            cr.set_source_rgba(0.0, 0.0, 0.0, 0.5);
            cr.set_line_width(7.0);
            let _ = cr.stroke_preserve();
            cr.set_source_rgb(r * 0.4 + 0.6, g * 0.4 + 0.6, b * 0.4 + 0.6);
            cr.set_line_width(4.0);
            let _ = cr.stroke();
        }

        // Axis letters at the ends of the coloured corner markers.
        let corners = region.corners().map(|c| Vec3::new(c[0] as f32, c[1] as f32, c[2] as f32));
        cr.select_font_face("Sans", gtk::cairo::FontSlant::Normal, gtk::cairo::FontWeight::Bold);
        cr.set_font_size(12.0);
        for (axis, (corner, label)) in [(1, "X"), (2, "Y"), (4, "Z")].into_iter().enumerate() {
            let end = corners[0].lerp(corners[corner], AXIS_MARKER_FRACTION);
            let (Some(o), Some(e)) = (to_pixels(&vp, corners[0], w, h), to_pixels(&vp, end, w, h)) else {
                continue;
            };
            let (dx, dy) = (e.0 - o.0, e.1 - o.1);
            let len = dx.hypot(dy);
            if len < 12.0 {
                continue;
            }
            let (x, y) = (e.0 + dx / len * 9.0 - 4.0, e.1 + dy / len * 9.0 + 4.5);
            let (r, g, b) = rgb(RING_COLORS[axis]);
            cr.move_to(x, y);
            cr.text_path(label);
            cr.set_source_rgba(0.0, 0.0, 0.0, 0.7);
            cr.set_line_width(3.0);
            let _ = cr.stroke_preserve();
            cr.set_source_rgb(r, g, b);
            let _ = cr.fill();
        }

        for (handle, base, tip) in visible_handles(region, &vp, w, h) {
            let hot = active == Some(handle);
            let radius = if hot { HANDLE_HOVER_RADIUS } else { HANDLE_DISC_RADIUS };
            let (r, g, b) = match handle {
                Handle::Face { axis, .. } => rgb(RING_COLORS[axis]),
                _ => rgb(REGION_COLOR),
            };
            if matches!(handle, Handle::Face { .. }) {
                cr.move_to(base.0, base.1);
                cr.line_to(tip.0, tip.1);
                cr.set_source_rgba(0.0, 0.0, 0.0, 0.5);
                cr.set_line_width(if hot { 5.0 } else { 3.5 });
                let _ = cr.stroke_preserve();
                cr.set_source_rgb(r, g, b);
                cr.set_line_width(if hot { 3.0 } else { 1.5 });
                let _ = cr.stroke();
                cr.arc(tip.0, tip.1, radius, 0.0, std::f64::consts::TAU);
            } else {
                cr.rectangle(tip.0 - radius, tip.1 - radius, radius * 2.0, radius * 2.0);
            }
            if hot {
                cr.set_source_rgb(1.0, 1.0, 1.0);
            } else {
                cr.set_source_rgb(r, g, b);
            }
            let _ = cr.fill_preserve();
            cr.set_source_rgba(0.0, 0.0, 0.0, 0.75);
            cr.set_line_width(if hot { 2.5 } else { 1.5 });
            let _ = cr.stroke();
        }
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
        self.redraw();
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
            st.camera = OrbitCamera {
                orthographic: st.camera.orthographic,
                ..OrbitCamera::framing(lo, hi, st.camera.up)
            };
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
        self.redraw();
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
        self.redraw();
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
        self.redraw();
    }

    /// Developer hook: drag a gizmo handle by `dx`/`dy` pixels as if with the mouse.
    pub fn script_region_drag(&self, handle: &str, dx: f64, dy: f64) {
        let Some(handle) = parse_handle(handle) else { return };
        let region = {
            let mut st = self.state.borrow_mut();
            let Some((vp, w, h)) = self.view_projection(&st) else { return };
            let Some(pos) = st.region_model.as_ref().and_then(|r| {
                let p = match handle {
                    // A quarter of the way round the ring, where it is seldom edge-on.
                    Handle::Ring { axis } => Some(ring_points(r, axis)[RING_SEGMENTS / 8]),
                    _ => region_handles(r).into_iter().find(|(hh, ..)| *hh == handle).map(|(.., p)| p),
                };
                p.and_then(|p| to_pixels(&vp, p, w, h))
            }) else {
                return;
            };
            st.region_drag = self.begin_region_drag(&st, handle, pos);
            Viewport::update_region_drag(&mut st, (pos.0 + dx, pos.1 + dy));
            st.region_drag = None;
            st.region_model.clone()
        };
        self.redraw();
        if let (Some(region), Some(cb)) = (region, self.on_region_changed.borrow().as_ref()) {
            cb(region);
        }
    }

    /// Developer hook: highlight a gizmo handle as if the pointer rested on it (`None` clears).
    pub fn script_hover(&self, handle: Option<&str>) {
        let mut st = self.state.borrow_mut();
        st.hover = handle.and_then(parse_handle);
        rebuild_region(&mut st);
        drop(st);
        self.redraw();
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
        for (handle, _, (px, py)) in visible_handles(region, &vp, w, h) {
            let d = (px - x).hypot(py - y);
            if d <= PICK_RADIUS * 0.8 && best.is_none_or(|(bd, _)| d < bd) {
                best = Some((d, handle));
            }
        }
        if best.is_some() {
            // Face and centre handles win over a ring passing nearby.
            return best.map(|(_, h)| h);
        }
        for axis in 0..3 {
            let pts: Vec<_> =
                ring_points(region, axis).into_iter().map(|p| to_pixels(&vp, p, w, h)).collect();
            for pair in pts.windows(2) {
                let (Some(a), Some(b)) = (pair[0], pair[1]) else { continue };
                let d = point_segment_distance((x, y), a, b);
                if d <= RING_PICK_RADIUS && best.is_none_or(|(bd, _)| d < bd) {
                    best = Some((d, Handle::Ring { axis }));
                }
            }
        }
        best.map(|(_, h)| h)
    }

    fn begin_region_drag(&self, st: &State, handle: Handle, pointer: (f64, f64)) -> Option<RegionDrag> {
        let region = st.region_model.clone()?;
        let (vp, w, h) = self.view_projection(st)?;
        let axes = region_axes(&region);
        let centre = region_centre(&region);
        let diag = region_diagonal(&region);
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
            Handle::Ring { axis } => (axes[axis], (0.0, 0.0)),
        };
        let view = st.camera.view();
        let centre_px = to_pixels(&vp, centre, w, h)?;
        let start_angle = (pointer.1 - centre_px.1).atan2(pointer.0 - centre_px.0);
        let towards_viewer = view.transform_vector3(axis).z >= 0.0;
        let right = Vec3::new(view.x_axis.x, view.y_axis.x, view.z_axis.x);
        let up = Vec3::new(view.x_axis.y, view.y_axis.y, view.z_axis.y);
        let depth = -view.transform_point3(centre).z;
        let world_per_px = st.camera.world_per_pixel(depth, h as f32);
        Some(RegionDrag {
            handle,
            start: region,
            start_pointer: pointer,
            axis,
            px_per_unit,
            world_per_px,
            right,
            up,
            centre_px,
            start_angle,
            towards_viewer,
            snap: false,
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
            Handle::Ring { .. } => {
                let (cx, cy) = drag.centre_px;
                if (pointer.0 - cx).hypot(pointer.1 - cy) < 4.0 {
                    return;
                }
                // Screen angles grow clockwise (y points down); a rotation about an axis that
                // points at the viewer turns counter-clockwise on screen.
                let mut delta = (pointer.1 - cy).atan2(pointer.0 - cx) - drag.start_angle;
                delta =
                    (delta + std::f64::consts::PI).rem_euclid(std::f64::consts::TAU) - std::f64::consts::PI;
                let mut angle = if drag.towards_viewer { -delta } else { delta };
                if drag.snap {
                    let step = 15f64.to_radians();
                    angle = (angle / step).round() * step;
                }
                region.set_rotation(mat_mul(axis_angle_matrix(drag.axis, angle), drag.start.rotation()));
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
                st.camera = OrbitCamera {
                    orthographic: st.camera.orthographic,
                    ..OrbitCamera::framing(Vec3::from(lo), Vec3::from(hi), st.camera.up)
                };
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
        self.redraw();
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
        self.redraw();
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
        self.redraw();
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
        self.redraw();
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
        self.redraw();
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
        self.redraw();
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
        self.redraw();
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
        self.redraw();
    }

    /// Look along a world axis (Top, Front, …), keeping the target and zoom.
    pub fn set_view_preset(&self, preset: ViewPreset) {
        self.state.borrow_mut().camera.set_preset(preset);
        self.redraw();
    }

    /// Switch between perspective and parallel projection.
    pub fn toggle_orthographic(&self) {
        self.ortho_toggle.set_active(!self.ortho_toggle.is_active());
    }

    pub fn set_orthographic(&self, on: bool) {
        self.ortho_toggle.set_active(on);
    }

    /// Orbit by `dx`/`dy` pixels and zoom by `factor` (for scripted views).
    pub fn nudge(&self, dx: f32, dy: f32, factor: f32) {
        let mut st = self.state.borrow_mut();
        st.camera.orbit(dx, dy);
        st.camera.zoom(factor);
        drop(st);
        self.redraw();
    }

    /// Re-frame whatever is shown: the mesh if there is one, otherwise the sparse scene.
    pub fn reset_view(&self) {
        let mut st = self.state.borrow_mut();
        let bounds = match (&st.mesh, st.show_mesh) {
            (Some(m), true) => m.bounds().map(|(lo, hi)| (Vec3::from(lo), Vec3::from(hi))),
            _ => st.focus,
        };
        if let Some((lo, hi)) = bounds {
            st.camera = OrbitCamera {
                orthographic: st.camera.orthographic,
                ..OrbitCamera::framing(lo, hi, st.camera.up)
            };
        }
        drop(st);
        self.redraw();
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

#[cfg(test)]
mod ring_tests {
    use super::*;

    #[test]
    fn nav_cube_shows_the_face_of_the_current_view() {
        for (preset, _) in NAV_CUBE_FACES {
            let mut cam = OrbitCamera { up: Vec3::Z, ..OrbitCamera::default() };
            cam.set_preset(preset);
            assert_eq!(nav_cube_pick(&cam, 110.0, 110.0, 55.0, 55.0), Some(preset));
            // Its label reads left to right and top to bottom (not mirrored or upside down).
            let faces = nav_cube_faces(&cam, 110.0, 110.0);
            let face = faces.last().unwrap();
            assert_eq!(face.preset, preset);
            assert!(face.right.0 > 30.0 && face.right.1.abs() < 1.0, "{preset:?} {:?}", face.right);
            assert!(face.down.1 > 30.0 && face.down.0.abs() < 1.0, "{preset:?} {:?}", face.down);
        }
    }

    #[test]
    fn handles_facing_the_viewer_are_hidden() {
        let r = Region::new([0.0; 3], [2.0, 2.0, 2.0], [0.0, 0.0, 0.0]);
        let mut cam = OrbitCamera {
            up: Vec3::Z,
            orthographic: true,
            ..OrbitCamera::framing(Vec3::splat(-1.0), Vec3::ONE, Vec3::Z)
        };
        cam.set_preset(ViewPreset::Front);
        let vp = cam.view_projection(1.0);
        let handles: Vec<Handle> =
            visible_handles(&r, &vp, 400.0, 400.0).into_iter().map(|(h, ..)| h).collect();
        // Looking along Y: the ±Y face handles point at the viewer; the other four and the
        // centre stand clear of the box outline.
        assert_eq!(handles.len(), 5, "{handles:?}");
        assert!(!handles.iter().any(|h| matches!(h, Handle::Face { axis: 1, .. })));
    }

    #[test]
    fn ground_face_is_the_local_bottom() {
        let r = Region::new([0.0, 0.0, 5.0], [2.0, 4.0, 10.0], [0.0; 3]);
        let segs = region_segments(&r, None, false);
        let ground: Vec<_> = segs.iter().filter(|s| s.color == GROUND_FACE_COLOR).collect();
        assert_eq!(ground.len(), 4);
        assert!(ground.iter().all(|s| s.a.z.abs() < 1e-5 && s.b.z.abs() < 1e-5));
        let grid = segs.iter().filter(|s| s.color == GROUND_GRID_COLOR).count();
        assert_eq!(grid, 2 * (GROUND_GRID_CELLS - 1));
    }

    #[test]
    fn ring_rotation_turns_about_the_local_axis() {
        let start = Region::new([1.0, 2.0, 3.0], [2.0, 3.0, 4.0], [30.0, 10.0, -20.0]);
        for axis in 0..3 {
            let n = region_axes(&start)[axis];
            let mut r = start.clone();
            r.set_rotation(mat_mul(axis_angle_matrix(n, 0.7), start.rotation()));
            let after = region_axes(&r);
            // The ring's own axis stays put, the other two turn by the angle.
            assert!((after[axis] - n).length() < 1e-4, "axis {axis}");
            let other = (axis + 1) % 3;
            let cos = after[other].dot(region_axes(&start)[other]);
            assert!((cos - 0.7f32.cos()).abs() < 1e-4, "axis {axis}: cos {cos}");
        }
    }
}
