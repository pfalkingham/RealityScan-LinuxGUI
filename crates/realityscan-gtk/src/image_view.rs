//! 2D image view: a zoomable, pannable full-resolution image with control-point
//! measurements drawn on top. A click (with an active control point) places a measurement.
//!
//! The image is a GPU texture drawn through `snapshot`, so 45-megapixel frames pan and
//! zoom smoothly; markers are drawn with cairo on top.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gtk4 as gtk;

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene};

use crate::mask::MaskLayer;

/// What a left drag does when a mask is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaskMode {
    Off,
    /// Paint masked-out (excluded) area.
    Paint,
    /// Restore (keep) area.
    Erase,
}

/// A measurement to draw: pixel position, label, whether it belongs to the active point.
#[derive(Debug, Clone, PartialEq)]
pub struct Marker {
    pub x: f64,
    pub y: f64,
    pub label: String,
    pub active: bool,
    /// Not a measurement in this image but where the triangulated point projects to: drawn
    /// hollow, can't be dragged or removed, and a click on it places a measurement there.
    pub projected: bool,
}

type PlaceFn = Rc<RefCell<Option<Box<dyn Fn(f64, f64)>>>>;
type RemoveFn = Rc<RefCell<Option<Box<dyn Fn(String)>>>>;
type MoveFn = Rc<RefCell<Option<Box<dyn Fn(String, f64, f64)>>>>;
type MaskFn = Rc<RefCell<Option<Box<dyn Fn(PathBuf)>>>>;
type ClearFn = Rc<RefCell<Option<Box<dyn Fn()>>>>;
type Decoded = Result<(u32, u32, Vec<u8>), String>;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Canvas {
        pub texture: RefCell<Option<gdk::Texture>>,
        pub image_size: Cell<(f64, f64)>,
        /// Zoom (screen pixels per image pixel) and the image origin in widget pixels.
        pub scale: Cell<f64>,
        pub offset: Cell<(f64, f64)>,
        pub markers: RefCell<Vec<Marker>>,
        pub fitted: Cell<bool>,
        pub mask_texture: RefCell<Option<gdk::Texture>>,
        pub show_mask: Cell<bool>,
        /// Brush cursor to draw: (image x, image y, radius in image px).
        pub brush: Cell<Option<(f64, f64, f64)>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Canvas {
        const NAME: &'static str = "RsImageCanvas";
        type Type = super::Canvas;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for Canvas {
        fn constructed(&self) {
            self.parent_constructed();
            self.scale.set(1.0);
            self.obj().set_hexpand(true);
            self.obj().set_vexpand(true);
        }
    }

    impl WidgetImpl for Canvas {
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let obj = self.obj();
            let (w, h) = (obj.width() as f32, obj.height() as f32);
            snapshot
                .append_color(&gdk::RGBA::new(0.08, 0.08, 0.09, 1.0), &graphene::Rect::new(0.0, 0.0, w, h));
            let Some(texture) = self.texture.borrow().clone() else { return };
            if !self.fitted.get() {
                obj.fit();
            }
            let s = self.scale.get();
            let (ox, oy) = self.offset.get();
            let (iw, ih) = self.image_size.get();
            let rect = graphene::Rect::new(ox as f32, oy as f32, (iw * s) as f32, (ih * s) as f32);
            snapshot.append_scaled_texture(&texture, gtk::gsk::ScalingFilter::Trilinear, &rect);
            if self.show_mask.get()
                && let Some(mask) = self.mask_texture.borrow().as_ref()
            {
                snapshot.append_scaled_texture(mask, gtk::gsk::ScalingFilter::Linear, &rect);
            }

            let markers = self.markers.borrow();
            let brush = self.brush.get();
            if markers.is_empty() && brush.is_none() {
                return;
            }
            let cr = snapshot.append_cairo(&graphene::Rect::new(0.0, 0.0, w, h));
            if let Some((bx, by, br)) = brush {
                cr.set_source_rgba(1.0, 1.0, 1.0, 0.9);
                cr.set_line_width(1.0);
                cr.arc(ox + bx * s, oy + by * s, (br * s).max(2.0), 0.0, std::f64::consts::TAU);
                let _ = cr.stroke();
            }
            cr.set_line_width(1.5);
            for m in markers.iter() {
                // Text leaves a current point behind, and `arc` would draw a line from it to
                // the next circle: every marker starts from an empty path.
                cr.new_path();
                let (px, py) = (ox + m.x * s, oy + m.y * s);
                if px < -40.0 || py < -40.0 || px > w as f64 + 40.0 || py > h as f64 + 40.0 {
                    continue;
                }
                let (r, g, b) = if m.active { (1.0, 0.85, 0.2) } else { (0.3, 0.8, 1.0) };
                if m.projected {
                    // Where the point should be, according to the other images: dashed.
                    cr.set_source_rgba(0.0, 0.0, 0.0, 0.5);
                    cr.set_line_width(3.0);
                    cr.set_dash(&[4.0, 3.0], 0.0);
                    cr.arc(px, py, 11.0, 0.0, std::f64::consts::TAU);
                    let _ = cr.stroke();
                    cr.set_source_rgba(r, g, b, 0.9);
                    cr.set_line_width(1.5);
                    cr.arc(px, py, 11.0, 0.0, std::f64::consts::TAU);
                    let _ = cr.stroke();
                    cr.set_dash(&[], 0.0);
                    cr.move_to(px + 13.0, py - 11.0);
                    cr.set_font_size(12.0);
                    cr.set_source_rgba(0.0, 0.0, 0.0, 0.7);
                    let _ = cr.show_text(&format!("{}?", m.label));
                    cr.move_to(px + 12.0, py - 12.0);
                    cr.set_source_rgba(r, g, b, 0.9);
                    let _ = cr.show_text(&format!("{}?", m.label));
                    continue;
                }
                cr.set_source_rgba(0.0, 0.0, 0.0, 0.6);
                cr.set_line_width(3.0);
                cr.arc(px, py, 9.0, 0.0, std::f64::consts::TAU);
                let _ = cr.stroke();
                cr.set_source_rgb(r, g, b);
                cr.set_line_width(1.5);
                cr.arc(px, py, 9.0, 0.0, std::f64::consts::TAU);
                let _ = cr.stroke();
                for (dx, dy) in [(1.0, 0.0), (-1.0, 0.0), (0.0, 1.0), (0.0, -1.0)] {
                    cr.move_to(px + dx * 4.0, py + dy * 4.0);
                    cr.line_to(px + dx * 14.0, py + dy * 14.0);
                }
                let _ = cr.stroke();
                cr.move_to(px + 13.0, py - 11.0);
                cr.set_font_size(13.0);
                cr.set_source_rgba(0.0, 0.0, 0.0, 0.7);
                let _ = cr.show_text(&m.label);
                cr.move_to(px + 12.0, py - 12.0);
                cr.set_source_rgb(r, g, b);
                let _ = cr.show_text(&m.label);
            }
        }
    }
}

glib::wrapper! {
    pub struct Canvas(ObjectSubclass<imp::Canvas>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for Canvas {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl Canvas {
    pub fn set_texture(&self, texture: Option<gdk::Texture>) {
        let imp = self.imp();
        if let Some(t) = &texture {
            imp.image_size.set((t.width() as f64, t.height() as f64));
        }
        *imp.texture.borrow_mut() = texture;
        imp.fitted.set(false);
        self.queue_draw();
    }

    pub fn set_markers(&self, markers: Vec<Marker>) {
        *self.imp().markers.borrow_mut() = markers;
        self.queue_draw();
    }

    pub fn set_mask_texture(&self, texture: Option<gdk::Texture>) {
        *self.imp().mask_texture.borrow_mut() = texture;
        self.queue_draw();
    }

    pub fn set_show_mask(&self, show: bool) {
        self.imp().show_mask.set(show);
        self.queue_draw();
    }

    pub fn set_brush(&self, brush: Option<(f64, f64, f64)>) {
        self.imp().brush.set(brush);
        self.queue_draw();
    }

    pub fn image_size(&self) -> (f64, f64) {
        self.imp().image_size.get()
    }

    /// Zoom so the whole image is visible, centred.
    pub fn fit(&self) {
        let imp = self.imp();
        let (iw, ih) = imp.image_size.get();
        let (w, h) = (self.width() as f64, self.height() as f64);
        if iw <= 0.0 || ih <= 0.0 || w <= 0.0 || h <= 0.0 {
            return;
        }
        let s = (w / iw).min(h / ih) * 0.98;
        imp.scale.set(s);
        imp.offset.set(((w - iw * s) * 0.5, (h - ih * s) * 0.5));
        imp.fitted.set(true);
        self.queue_draw();
    }

    pub fn scale(&self) -> f64 {
        self.imp().scale.get()
    }

    /// Zoom by `factor` keeping the image point under (`x`, `y`) fixed.
    pub fn zoom_at(&self, factor: f64, x: f64, y: f64) {
        let imp = self.imp();
        let s0 = imp.scale.get();
        let s1 = (s0 * factor).clamp(0.01, 16.0);
        let (ox, oy) = imp.offset.get();
        let (ix, iy) = ((x - ox) / s0, (y - oy) / s0);
        imp.scale.set(s1);
        imp.offset.set((x - ix * s1, y - iy * s1));
        imp.fitted.set(true);
        self.queue_draw();
    }

    pub fn pan(&self, dx: f64, dy: f64) {
        let imp = self.imp();
        let (ox, oy) = imp.offset.get();
        imp.offset.set((ox + dx, oy + dy));
        self.queue_draw();
    }

    /// Widget coordinates → image pixel coordinates, if inside the image.
    pub fn to_image(&self, x: f64, y: f64) -> Option<(f64, f64)> {
        let imp = self.imp();
        let s = imp.scale.get();
        let (ox, oy) = imp.offset.get();
        let (iw, ih) = imp.image_size.get();
        let (ix, iy) = ((x - ox) / s, (y - oy) / s);
        (ix >= 0.0 && iy >= 0.0 && ix < iw && iy < ih).then_some((ix, iy))
    }
}

/// The image view page: header row, canvas, status line.
#[derive(Clone)]
pub struct ImageView {
    root: gtk::Box,
    canvas: Canvas,
    title: gtk::Label,
    hint: gtk::Label,
    zoom_label: gtk::Label,
    spinner: adw::Spinner,
    back: gtk::Button,
    /// Image currently shown (local path and RealityScan path).
    current: Rc<RefCell<Option<(PathBuf, String)>>>,
    /// Decode generation, so a stale decode can't replace a newer image.
    generation: Rc<Cell<u64>>,
    on_place: PlaceFn,
    on_remove: RemoveFn,
    on_move: MoveFn,
    /// Marker being dragged: (label, original x, original y).
    dragging: Rc<RefCell<Option<(String, f64, f64)>>>,
    decode_tx: async_channel::Sender<(u64, Decoded)>,
    // Masking
    mask: Rc<RefCell<Option<MaskLayer>>>,
    mask_mode: Rc<Cell<MaskMode>>,
    /// Mask file to load once the image size is known.
    pending_mask: Rc<RefCell<Option<PathBuf>>>,
    /// Where applied masks are written (`<dir>/<image stem>.png`).
    mask_dir: Rc<RefCell<Option<PathBuf>>>,
    mask_toggle: gtk::ToggleButton,
    paint_toggle: gtk::ToggleButton,
    erase_toggle: gtk::ToggleButton,
    brush_size: gtk::SpinButton,
    mask_tools: gtk::Box,
    mask_info: gtk::Label,
    painting: Rc<Cell<bool>>,
    last_paint: Rc<Cell<Option<(f64, f64)>>>,
    last_texture_update: Rc<Cell<Option<std::time::Instant>>>,
    on_apply_mask: MaskFn,
    on_clear_mask: ClearFn,
}

use libadwaita as adw;

impl ImageView {
    pub fn new() -> Self {
        let canvas = Canvas::default();
        let back = gtk::Button::from_icon_name("go-previous-symbolic");
        back.set_tooltip_text(Some("Back to the 3D view"));
        back.add_css_class("flat");
        let title = gtk::Label::builder()
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .build();
        title.add_css_class("heading");
        let hint = gtk::Label::builder().xalign(0.0).build();
        hint.add_css_class("dim-label");
        let zoom_label = gtk::Label::new(None);
        zoom_label.add_css_class("dim-label");
        zoom_label.add_css_class("numeric");
        let fit = gtk::Button::from_icon_name("zoom-fit-best-symbolic");
        fit.set_tooltip_text(Some("Fit the image"));
        fit.add_css_class("flat");
        let spinner = adw::Spinner::new();
        spinner.set_visible(false);
        let header = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .margin_start(6)
            .margin_end(10)
            .margin_top(4)
            .margin_bottom(4)
            .build();
        header.append(&back);
        header.append(&title);
        header.append(&spinner);
        header.append(&zoom_label);
        header.append(&fit);
        // Mask tools
        let mask_toggle = gtk::ToggleButton::builder()
            .label("Mask")
            .tooltip_text("Show and edit this image's mask")
            .build();
        let paint_toggle = gtk::ToggleButton::builder()
            .icon_name("edit-select-all-symbolic")
            .tooltip_text("Paint: exclude the area under the brush (left drag)")
            .build();
        let erase_toggle = gtk::ToggleButton::builder()
            .icon_name("edit-clear-symbolic")
            .tooltip_text("Erase: put the area under the brush back (or right-drag)")
            .group(&paint_toggle)
            .build();
        let brush_size = gtk::SpinButton::with_range(5.0, 2000.0, 5.0);
        brush_size.set_value(120.0);
        brush_size.set_tooltip_text(Some("Brush radius in image pixels"));
        let apply = gtk::Button::builder()
            .label("Apply mask")
            .tooltip_text("Send this mask to RealityScan for the image")
            .build();
        apply.add_css_class("suggested-action");
        let clear =
            gtk::Button::builder().label("Clear").tooltip_text("Remove the mask from this image").build();
        let mask_info = gtk::Label::new(None);
        mask_info.add_css_class("dim-label");
        mask_info.add_css_class("caption");
        let mask_tools =
            gtk::Box::builder().orientation(gtk::Orientation::Horizontal).spacing(0).visible(false).build();
        mask_tools.add_css_class("linked");
        mask_tools.append(&paint_toggle);
        mask_tools.append(&erase_toggle);
        let ai = gtk::Button::builder()
            .label("AI mask")
            .action_name("win.generate-mask-current")
            .tooltip_text("Let RealityScan isolate the object in this image (about 40 s)")
            .build();
        let mask_edit =
            gtk::Box::builder().orientation(gtk::Orientation::Horizontal).spacing(8).visible(false).build();
        mask_edit.append(&ai);
        mask_edit.append(&brush_size);
        mask_edit.append(&apply);
        mask_edit.append(&clear);
        mask_edit.append(&mask_info);
        header.append(&mask_toggle);
        header.append(&mask_tools);
        header.append(&mask_edit);

        let status = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .margin_start(12)
            .margin_bottom(4)
            .build();
        status.append(&hint);
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.append(&header);
        root.append(&canvas);
        root.append(&status);

        let (decode_tx, decode_rx) = async_channel::unbounded();
        let this = Self {
            root,
            canvas,
            title,
            hint,
            zoom_label,
            spinner,
            back,
            current: Rc::new(RefCell::new(None)),
            generation: Rc::new(Cell::new(0)),
            on_place: Rc::new(RefCell::new(None)),
            on_remove: Rc::new(RefCell::new(None)),
            on_move: Rc::new(RefCell::new(None)),
            dragging: Rc::new(RefCell::new(None)),
            decode_tx,
            mask: Rc::new(RefCell::new(None)),
            mask_mode: Rc::new(Cell::new(MaskMode::Off)),
            pending_mask: Rc::new(RefCell::new(None)),
            mask_dir: Rc::new(RefCell::new(None)),
            mask_toggle,
            paint_toggle,
            erase_toggle,
            brush_size,
            mask_tools,
            mask_info,
            painting: Rc::new(Cell::new(false)),
            last_paint: Rc::new(Cell::new(None)),
            last_texture_update: Rc::new(Cell::new(None)),
            on_apply_mask: Rc::new(RefCell::new(None)),
            on_clear_mask: Rc::new(RefCell::new(None)),
        };
        // Mask tool behaviour
        this.mask_toggle.connect_toggled({
            let this = this.clone();
            let mask_edit = mask_edit.clone();
            move |t| {
                let on = t.is_active();
                this.mask_tools.set_visible(on);
                mask_edit.set_visible(on);
                if on {
                    this.ensure_mask();
                    if !this.paint_toggle.is_active() && !this.erase_toggle.is_active() {
                        this.paint_toggle.set_active(true);
                    }
                    this.mask_mode.set(if this.erase_toggle.is_active() {
                        MaskMode::Erase
                    } else {
                        MaskMode::Paint
                    });
                } else {
                    this.mask_mode.set(MaskMode::Off);
                    this.canvas.set_brush(None);
                }
                this.canvas.set_show_mask(on || this.mask.borrow().as_ref().is_some_and(|m| m.modified));
                this.update_mask_info();
            }
        });
        this.paint_toggle.connect_toggled({
            let this = this.clone();
            move |t| {
                if t.is_active() && this.mask_toggle.is_active() {
                    this.mask_mode.set(MaskMode::Paint);
                }
            }
        });
        this.erase_toggle.connect_toggled({
            let this = this.clone();
            move |t| {
                if t.is_active() && this.mask_toggle.is_active() {
                    this.mask_mode.set(MaskMode::Erase);
                }
            }
        });
        apply.connect_clicked({
            let this = this.clone();
            move |_| this.apply_mask()
        });
        clear.connect_clicked({
            let this = this.clone();
            move |_| {
                let (w, h) = this.canvas.image_size();
                *this.mask.borrow_mut() = Some(MaskLayer::new(w as u32, h as u32));
                this.refresh_mask_texture(true);
                this.update_mask_info();
                if let Some(cb) = this.on_clear_mask.borrow().as_ref() {
                    cb();
                }
            }
        });

        // Decoded images arrive here and become textures.
        glib::spawn_future_local({
            let this = this.clone();
            async move {
                while let Ok((generation, result)) = decode_rx.recv().await {
                    if generation != this.generation.get() {
                        continue;
                    }
                    this.spinner.set_visible(false);
                    match result {
                        Ok((w, h, rgb)) => {
                            let bytes = glib::Bytes::from_owned(rgb);
                            let texture = gdk::MemoryTexture::new(
                                w as i32,
                                h as i32,
                                gdk::MemoryFormat::R8g8b8,
                                &bytes,
                                (w * 3) as usize,
                            );
                            this.canvas.set_texture(Some(texture.upcast()));
                            this.update_zoom_label();
                            // A cached/exported mask for this image?
                            let pending = this.pending_mask.borrow_mut().take();
                            let loaded = pending.and_then(|p| MaskLayer::load(&p, w, h));
                            let has = loaded.is_some();
                            *this.mask.borrow_mut() = loaded;
                            this.refresh_mask_texture(true);
                            if has {
                                this.canvas.set_show_mask(true);
                            }
                            this.update_mask_info();
                        }
                        Err(e) => this.hint.set_label(&format!("Couldn't load the image: {e}")),
                    }
                }
            }
        });

        fit.connect_clicked({
            let this = this.clone();
            move |_| {
                this.canvas.fit();
                this.update_zoom_label();
            }
        });

        // Wheel zooms around the pointer.
        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        let pointer = Rc::new(Cell::new((0.0f64, 0.0f64)));
        let motion = gtk::EventControllerMotion::new();
        motion.connect_motion({
            let pointer = pointer.clone();
            let this = this.clone();
            move |_, x, y| {
                pointer.set((x, y));
                if this.mask_mode.get() != MaskMode::Off {
                    let r = this.brush_size.value();
                    this.canvas.set_brush(this.canvas.to_image(x, y).map(|(ix, iy)| (ix, iy, r)));
                }
                match this.canvas.to_image(x, y) {
                    Some((ix, iy)) => this
                        .zoom_label
                        .set_label(&format!("{:.0} %  ·  {ix:.0}, {iy:.0} px", this.canvas.scale() * 100.0)),
                    None => this.update_zoom_label(),
                }
            }
        });
        this.canvas.add_controller(motion);
        scroll.connect_scroll({
            let this = this.clone();
            let pointer = pointer.clone();
            move |_, _dx, dy| {
                let (x, y) = pointer.get();
                this.canvas.zoom_at(1.2f64.powf(-dy), x, y);
                this.update_zoom_label();
                glib::Propagation::Stop
            }
        });
        this.canvas.add_controller(scroll);

        // Drag pans; a click (no movement) places a measurement.
        let drag = gtk::GestureDrag::new();
        drag.set_button(0);
        let last = Rc::new(Cell::new((0.0f64, 0.0f64)));
        let moved = Rc::new(Cell::new(0.0f64));
        drag.connect_drag_begin({
            let this = this.clone();
            let last = last.clone();
            let moved = moved.clone();
            move |gesture, x, y| {
                last.set((x, y));
                moved.set(0.0);
                *this.dragging.borrow_mut() = None;
                // With the mask shown, the left (paint) or right (erase) button paints.
                let mode = this.mask_mode.get();
                let button = gesture.current_button();
                if mode != MaskMode::Off && (button == 1 || button == 3) {
                    this.painting.set(true);
                    this.last_paint.set(None);
                    this.paint_at(x, y, button == 3);
                    return;
                }
                // A left press on a marker starts moving that measurement.
                if gesture.current_button() == 1
                    && let Some(label) = this.marker_at(x, y)
                {
                    let orig = this
                        .canvas
                        .imp()
                        .markers
                        .borrow()
                        .iter()
                        .find(|m| m.label == label)
                        .map(|m| (m.x, m.y));
                    if let Some((mx, my)) = orig {
                        *this.dragging.borrow_mut() = Some((label, mx, my));
                    }
                }
            }
        });
        drag.connect_drag_update({
            let this = this.clone();
            let last = last.clone();
            let moved = moved.clone();
            move |gesture, dx, dy| {
                let Some((sx, sy)) = gesture.start_point() else { return };
                let (x, y) = (sx + dx, sy + dy);
                let (lx, ly) = last.get();
                last.set((x, y));
                moved.set(moved.get().max(dx.hypot(dy)));
                if this.painting.get() {
                    this.paint_at(x, y, gesture.current_button() == 3);
                    return;
                }
                let dragging = this.dragging.borrow().clone();
                match dragging {
                    Some((label, ox, oy)) => {
                        let s = this.canvas.scale();
                        let (nx, ny) = (ox + dx / s, oy + dy / s);
                        let mut markers = this.canvas.imp().markers.borrow_mut();
                        if let Some(m) = markers.iter_mut().find(|m| m.label == label) {
                            m.x = nx;
                            m.y = ny;
                        }
                        drop(markers);
                        this.canvas.queue_draw();
                    }
                    None => this.canvas.pan(x - lx, y - ly),
                }
            }
        });
        drag.connect_drag_end({
            let this = this.clone();
            let moved = moved.clone();
            move |gesture, dx, dy| {
                if this.painting.replace(false) {
                    this.refresh_mask_texture(true);
                    this.update_mask_info();
                    return;
                }
                if gesture.current_button() != 1 {
                    return;
                }
                let dragging = this.dragging.borrow_mut().take();
                if let Some((label, ox, oy)) = dragging {
                    if moved.get() > 3.0 {
                        let s = this.canvas.scale();
                        if let Some(cb) = this.on_move.borrow().as_ref() {
                            cb(label, ox + dx / s, oy + dy / s);
                        }
                    }
                    return;
                }
                if moved.get() > 3.0 {
                    return;
                }
                let Some((x, y)) = gesture.start_point() else { return };
                if let Some((ix, iy)) = this.canvas.to_image(x, y)
                    && let Some(cb) = this.on_place.borrow().as_ref()
                {
                    cb(ix, iy);
                }
            }
        });
        this.canvas.add_controller(drag);

        // Right-click on a marker asks to remove that measurement.
        let right = gtk::GestureClick::new();
        right.set_button(3);
        right.connect_released({
            let this = this.clone();
            move |_, _, x, y| {
                if this.mask_mode.get() != MaskMode::Off {
                    return;
                }
                let hit = this.marker_at(x, y);
                if let (Some(label), Some(cb)) = (hit, this.on_remove.borrow().as_ref()) {
                    cb(label);
                }
            }
        });
        this.canvas.add_controller(right);
        this
    }

    /// Label of the marker within 12 px of a widget position.
    fn marker_at(&self, x: f64, y: f64) -> Option<String> {
        let imp = self.canvas.imp();
        let s = imp.scale.get();
        let (ox, oy) = imp.offset.get();
        imp.markers
            .borrow()
            .iter()
            .filter(|m| !m.projected)
            .filter_map(|m| {
                let d = (ox + m.x * s - x).hypot(oy + m.y * s - y);
                (d <= 12.0).then_some((d, m.label.clone()))
            })
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, l)| l)
    }

    /// Called with a marker's label when the user right-clicks it.
    pub fn connect_remove(&self, f: impl Fn(String) + 'static) {
        *self.on_remove.borrow_mut() = Some(Box::new(f));
    }

    fn ensure_mask(&self) {
        if self.mask.borrow().is_none() {
            let (w, h) = self.canvas.image_size();
            if w >= 1.0 && h >= 1.0 {
                *self.mask.borrow_mut() = Some(MaskLayer::new(w as u32, h as u32));
                self.refresh_mask_texture(true);
            }
        }
    }

    /// Paint a stroke segment from the last position to the widget position (`x`, `y`).
    fn paint_at(&self, x: f64, y: f64, erase_button: bool) {
        let Some((ix, iy)) = self.canvas.to_image(x, y) else { return };
        let keep = erase_button || self.mask_mode.get() == MaskMode::Erase;
        let r = self.brush_size.value();
        {
            let mut mask = self.mask.borrow_mut();
            let Some(mask) = mask.as_mut() else { return };
            match self.last_paint.get() {
                Some((lx, ly)) => {
                    let steps = (((ix - lx).hypot(iy - ly) / (r * 0.5)).ceil() as usize).clamp(1, 200);
                    for i in 1..=steps {
                        let t = i as f64 / steps as f64;
                        mask.paint(lx + (ix - lx) * t, ly + (iy - ly) * t, r, keep);
                    }
                }
                None => mask.paint(ix, iy, r, keep),
            }
        }
        self.last_paint.set(Some((ix, iy)));
        self.canvas.set_brush(Some((ix, iy, r)));
        self.refresh_mask_texture(false);
    }

    /// Upload the overlay texture; throttled during strokes unless `force`.
    fn refresh_mask_texture(&self, force: bool) {
        let now = std::time::Instant::now();
        if !force
            && let Some(t) = self.last_texture_update.get()
            && now.duration_since(t) < std::time::Duration::from_millis(60)
        {
            return;
        }
        let texture = self.mask.borrow_mut().as_mut().filter(|m| m.dirty || force).map(|m| m.texture());
        if let Some(t) = texture {
            self.canvas.set_mask_texture(Some(t));
            self.last_texture_update.set(Some(now));
        } else if self.mask.borrow().is_none() {
            self.canvas.set_mask_texture(None);
        }
    }

    fn update_mask_info(&self) {
        let text = match self.mask.borrow().as_ref() {
            Some(m) if m.modified => format!("{:.1} % masked out", m.masked_fraction() * 100.0),
            Some(_) => "no mask yet".to_owned(),
            None => String::new(),
        };
        self.mask_info.set_label(&text);
    }

    /// Save the mask as `<mask_dir>/<image stem>.png` and hand the path to the application.
    fn apply_mask(&self) {
        let Some(dir) = self.mask_dir.borrow().clone() else { return };
        let Some((local, _)) = self.current.borrow().clone() else { return };
        let stem =
            local.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "mask".into());
        let path = dir.join(format!("{stem}.png"));
        let saved = {
            let mask = self.mask.borrow();
            let Some(mask) = mask.as_ref() else { return };
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string()).and_then(|_| mask.save_png(&path))
        };
        match saved {
            Ok(()) => {
                if let Some(cb) = self.on_apply_mask.borrow().as_ref() {
                    cb(path);
                }
            }
            Err(e) => self.hint.set_label(&format!("Couldn't save the mask: {e}")),
        }
    }

    /// Where applied masks are written.
    pub fn set_mask_dir(&self, dir: PathBuf) {
        *self.mask_dir.borrow_mut() = Some(dir);
    }

    /// Called with the PNG path after the user applies a mask.
    pub fn connect_apply_mask(&self, f: impl Fn(PathBuf) + 'static) {
        *self.on_apply_mask.borrow_mut() = Some(Box::new(f));
    }

    /// Called when the user clears the mask.
    pub fn connect_clear_mask(&self, f: impl Fn() + 'static) {
        *self.on_clear_mask.borrow_mut() = Some(Box::new(f));
    }

    /// Developer hooks for masks: `on`, `paint x y r`, `erase x y r`, `apply`.
    pub fn script_mask(&self, step: &str, args: &[f64]) {
        match step {
            "on" => self.mask_toggle.set_active(true),
            "paint" | "erase" => {
                self.mask_toggle.set_active(true);
                if let [x, y, r] = args {
                    if let Some(m) = self.mask.borrow_mut().as_mut() {
                        m.paint(*x, *y, *r, step == "erase");
                    }
                    self.refresh_mask_texture(true);
                    self.update_mask_info();
                }
            }
            "apply" => self.apply_mask(),
            _ => {}
        }
    }

    /// Called with a marker's label and its new image position after a drag.
    pub fn connect_move(&self, f: impl Fn(String, f64, f64) + 'static) {
        *self.on_move.borrow_mut() = Some(Box::new(f));
    }

    /// Developer hook: move the marker `label` by (`dx`, `dy`) image pixels.
    pub fn script_move(&self, label: &str, dx: f64, dy: f64) {
        let orig = self.canvas.imp().markers.borrow().iter().find(|m| m.label == label).map(|m| (m.x, m.y));
        if let (Some((x, y)), Some(cb)) = (orig, self.on_move.borrow().as_ref()) {
            cb(label.to_owned(), x + dx, y + dy);
        }
    }

    pub fn widget(&self) -> &gtk::Box {
        &self.root
    }

    pub fn back_button(&self) -> &gtk::Button {
        &self.back
    }

    /// Called with image pixel coordinates when the user clicks the image.
    pub fn connect_place(&self, f: impl Fn(f64, f64) + 'static) {
        *self.on_place.borrow_mut() = Some(Box::new(f));
    }

    /// RealityScan path of the image on show.
    pub fn current_image(&self) -> Option<String> {
        self.current.borrow().as_ref().map(|(_, rs)| rs.clone())
    }

    pub fn set_hint(&self, text: &str) {
        self.hint.set_label(text);
    }

    pub fn set_markers(&self, markers: Vec<Marker>) {
        self.canvas.set_markers(markers);
    }

    fn update_zoom_label(&self) {
        self.zoom_label.set_label(&format!("{:.0} %", self.canvas.scale() * 100.0));
    }

    /// Show an image: `thumb` (if any) appears at once, the full decode replaces it. `mask`
    /// is a mask file to show for it, if one exists.
    pub fn show(
        &self,
        local: &Path,
        rs_path: &str,
        display_name: &str,
        thumb: Option<&Path>,
        mask: Option<PathBuf>,
    ) {
        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        *self.current.borrow_mut() = Some((local.to_path_buf(), rs_path.to_owned()));
        self.title.set_label(display_name);
        self.canvas.set_markers(Vec::new());
        *self.mask.borrow_mut() = None;
        *self.pending_mask.borrow_mut() = mask;
        self.canvas.set_mask_texture(None);
        self.canvas.set_brush(None);
        self.canvas.set_show_mask(self.mask_toggle.is_active());
        self.update_mask_info();
        if let Some(t) = thumb.and_then(|t| gdk::Texture::from_filename(t).ok()) {
            // Scale the thumbnail's coordinate space to the real image later; for now show it.
            self.canvas.set_texture(Some(t));
        } else {
            self.canvas.set_texture(None);
        }
        self.spinner.set_visible(true);
        let tx = self.decode_tx.clone();
        let path = local.to_path_buf();
        std::thread::Builder::new()
            .name("image-decode".into())
            .spawn(move || {
                let result = crate::thumbs::decode(&path)
                    .map(|img| {
                        let rgb = img.to_rgb8();
                        let (w, h) = rgb.dimensions();
                        (w, h, rgb.into_raw())
                    })
                    .map_err(|e| e.to_string());
                let _ = tx.send_blocking((generation, result));
            })
            .expect("spawn image decode");
    }
}
