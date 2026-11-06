//! Main window: header with the pipeline actions, image grid and scene lists in the
//! sidebar, 3D viewport, status bar with a collapsible log.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

use gtk4 as gtk;
use libadwaita as adw;

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use rs_client::Event;
use rs_scene::Scene;
use rs_scene::sparse::CameraPose;

use crate::align_panel::AlignPanel;
use crate::backend::{Cmd, Detail, EditMesh, Handle, ImageFlag, Msg, Selector, TextureOp};
use crate::image_view::{ImageView, Marker};
use crate::region_panel::RegionPanel;
use crate::thumbs::Thumbnails;
use crate::viewport::{LassoMode, Viewport};
use crate::{export_dialog, recent};
use rs_scene::controlpoints::ControlPointData;
use rs_scene::ortho::{OrthoProjection, OrthoRequest, Side};

/// Actions that send RealityScan commands; disabled while it is busy.
const PIPELINE_ACTIONS: &[&str] = &[
    "run-all",
    "align-with-settings",
    "mask-from-mesh",
    "generate-masks-selected",
    "generate-mask-current",
    "import-masks",
    "export-masks",
    "new-ortho",
    "draft-align",
    "detect-features",
    "generate-masks",
    "import-gcp",
    "export-gcp",
    "export-measurements",
    "export-sparse-cloud",
    "export-registration",
    "export-undistorted",
    "export-xmp",
    "select-triangles",
    "remove-selected",
    "cut-inside",
    "cut-outside",
    "fill-holes",
    "texture-op",
    "new-control-point",
    "define-distance",
    "detect-markers",
    "update-alignment",
    "merge-components",
    "export-component",
    "import-component",
    "open",
    "open-recent",
    "save",
    "save-as",
    "add-images",
    "add-folder",
    "select-all-images",
    "deselect-images",
    "image-flag",
    "align",
    "model",
    "model-preview",
    "model-high",
    "colorize",
    "texture",
    "show-mesh",
    "simplify",
    "smooth",
    "close-holes",
    "clean-model",
    "keep-largest",
    "export-model",
    "region-auto",
    "region-density",
];
/// Subset of the above that needs a model.
const MODEL_ACTIONS: &[&str] = &[
    "mask-from-mesh",
    "new-ortho",
    "colorize",
    "texture",
    "show-mesh",
    "simplify",
    "smooth",
    "close-holes",
    "clean-model",
    "keep-largest",
    "export-model",
    "texture-op",
    "select-triangles",
    "remove-selected",
    "cut-inside",
    "cut-outside",
    "fill-holes",
];
/// Subset that needs an aligned component.
const COMPONENT_ACTIONS: &[&str] = &[
    "model",
    "model-preview",
    "model-high",
    "region-auto",
    "region-density",
    "export-component",
    "update-alignment",
    "export-sparse-cloud",
    "export-registration",
    "export-undistorted",
    "export-xmp",
];

const IMAGE_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "tif", "tiff", "jxl", "heic", "dng", "exr", "webp"];

#[derive(Clone)]
pub struct MainWindow {
    window: adw::ApplicationWindow,
    inner: Rc<Inner>,
}

/// Names and GUIDs behind the sidebar's component and model rows, in row order.
#[derive(Default)]
struct SceneItems {
    components: Vec<(String, String)>,
    /// (guid, name, textured)
    models: Vec<(String, String, bool)>,
    model_menus: Vec<gtk::MenuButton>,
}

struct Inner {
    backend: Handle,
    actions: gio::SimpleActionGroup,
    title: adw::WindowTitle,
    spinner: adw::Spinner,
    status: gtk::Label,
    activity: gtk::Label,
    progress: gtk::ProgressBar,
    image_grid: gtk::FlowBox,
    image_stack: gtk::Stack,
    image_count: gtk::Label,
    /// Align, Model, Color, Texture; the next sensible one is highlighted.
    step_buttons: Vec<gtk::Widget>,
    thumbs: Thumbnails,
    /// Picture widgets waiting for (or showing) a thumbnail, by image path.
    pictures: RefCell<HashMap<PathBuf, gtk::Picture>>,
    /// (aligned badge, flags label) of each grid cell, in image order.
    cells: RefCell<Vec<(gtk::Label, gtk::Label)>>,
    components_list: gtk::ListBox,
    models_list: gtk::ListBox,
    cp_list: gtk::ListBox,
    constraint_list: gtk::ListBox,
    ortho_list: gtk::ListBox,
    orthos: RefCell<Vec<OrthoProjection>>,
    /// An ortho export headed for the image view, by path.
    pending_ortho_view: RefCell<Option<PathBuf>>,
    scene_items: RefCell<SceneItems>,
    cps: RefCell<ControlPointData>,
    /// Control point that clicks in the image view place measurements for.
    active_point: RefCell<Option<String>>,
    /// Control points hidden in the 3D view (eye toggle in their rows).
    hidden_points: RefCell<std::collections::HashSet<String>>,
    /// A point just placed by Ctrl+click on the mesh: (name, picked position), to report how
    /// far RealityScan's triangulation lands from it.
    pending_placement: RefCell<Option<(String, glam::Vec3)>>,
    image_view: ImageView,
    view_stack: gtk::Stack,
    region_panel: RegionPanel,
    align_panel: AlignPanel,
    /// Header/sidebar menu buttons by name, for the developer script hook.
    menu_buttons: HashMap<&'static str, gtk::MenuButton>,
    toasts: adw::ToastOverlay,
    recent_menu: gio::Menu,
    /// Radio list of the RealityScan runtimes found on this machine.
    runtime_menu: gio::Menu,
    log: gtk::TextBuffer,
    log_view: gtk::TextView,
    banner: adw::Banner,
    /// Offers to recover an autosave left by an earlier RealityScan.
    autosave_banner: adw::Banner,
    autosave_file: RefCell<Option<PathBuf>>,
    viewport: Viewport,
    scene: RefCell<Option<Scene>>,
    cameras: RefCell<Vec<CameraPose>>,
    /// File name of the open project, if it has been saved.
    project_file: RefCell<Option<String>>,
    last_recent: RefCell<Option<PathBuf>>,
    /// Project to open as soon as RealityScan is ready (from the command line).
    open_on_start: RefCell<Option<PathBuf>>,
    connected: Cell<bool>,
    /// We launched the RealityScan instance (so quitting the app quits it, and
    /// closing while busy deserves a warning).
    owned: Cell<bool>,
    busy: Cell<bool>,
    has_model: Cell<bool>,
    has_component: Cell<bool>,
    /// True while the sidebar lists are being rebuilt, so selection callbacks stay quiet.
    syncing: Cell<bool>,
    stopping: Cell<bool>,
    backend_done: Cell<bool>,
}

impl MainWindow {
    pub fn new(app: &adw::Application, backend: Handle, open_on_start: Option<PathBuf>) -> Self {
        let window = adw::ApplicationWindow::builder()
            .application(app)
            .default_width(1280)
            .default_height(820)
            .title("RealityScan")
            .build();

        // ----- header
        let title = adw::WindowTitle::new("RealityScan", "Starting…");
        let header = adw::HeaderBar::builder().title_widget(&title).build();
        let open = icon_button("document-open-symbolic", "Open project (Ctrl+O)", "win.open");
        let save = icon_button("document-save-symbolic", "Save project (Ctrl+S)", "win.save");
        let add_images = icon_button("list-add-symbolic", "Add images (Ctrl+I)", "win.add-images");
        let add_folder = icon_button("folder-open-symbolic", "Add a folder of images", "win.add-folder");
        for b in [&open, &save, &add_images, &add_folder] {
            header.pack_start(b);
        }
        let pipeline = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        pipeline.add_css_class("linked");
        let align_panel = AlignPanel::new();
        let align = adw::SplitButton::builder()
            .label("Align")
            .action_name("win.align")
            .popover(align_panel.popover())
            .tooltip_text("Detect features and align the images (arrow: alignment settings)")
            .build();
        let detail_menu = gio::Menu::new();
        detail_menu.append(Some("_Preview detail"), Some("win.model-preview"));
        detail_menu.append(Some("_Normal detail"), Some("win.model"));
        detail_menu.append(Some("_High detail"), Some("win.model-high"));
        let model = adw::SplitButton::builder()
            .label("Model")
            .action_name("win.model")
            .menu_model(&detail_menu)
            .tooltip_text(
                "Reconstruct a normal-detail model inside the region (menu for other detail levels)",
            )
            .build();
        let color_menu = gio::Menu::new();
        color_menu.append(Some("_Preview colours (fast)"), Some("win.texture-op::preview-colors"));
        color_menu.append(Some("_Quality colours"), Some("win.texture-op::quality-colors"));
        color_menu.append(Some("_Correct colours"), Some("win.texture-op::correct-colors"));
        let colorize = adw::SplitButton::builder()
            .label("Color")
            .action_name("win.colorize")
            .menu_model(&color_menu)
            .tooltip_text("Calculate vertex colours for the selected model (menu for variants)")
            .build();
        let texture_menu = gio::Menu::new();
        texture_menu.append(Some("_Unwrap only"), Some("win.texture-op::unwrap"));
        texture_menu.append(Some("_Quality texture"), Some("win.texture-op::quality-texture"));
        let texture = adw::SplitButton::builder()
            .label("Texture")
            .action_name("win.texture")
            .menu_model(&texture_menu)
            .tooltip_text("Unwrap and texture the selected model (menu for variants)")
            .build();
        pipeline.append(&align);
        pipeline.append(&model);
        pipeline.append(&colorize);
        pipeline.append(&texture);
        header.pack_start(&pipeline);
        let step_buttons: Vec<gtk::Widget> =
            vec![align.upcast(), model.upcast(), colorize.upcast(), texture.upcast()];

        let region_panel = RegionPanel::new();
        header.pack_start(region_panel.widget());

        let tools_menu = gio::Menu::new();
        let edit = gio::Menu::new();
        edit.append(Some("_Simplify…"), Some("win.simplify"));
        edit.append(Some("S_mooth"), Some("win.smooth"));
        edit.append(Some("Close _Holes"), Some("win.close-holes"));
        edit.append(Some("_Fill Holes"), Some("win.fill-holes"));
        edit.append(Some("_Clean Model"), Some("win.clean-model"));
        edit.append(Some("Keep _Largest Part Only"), Some("win.keep-largest"));
        tools_menu.append_section(Some("Selected model"), &edit);
        let selection = gio::Menu::new();
        for (label, target) in [
            ("Inside the Region", "inside"),
            ("Outside the Region", "outside"),
            ("Marginal Triangles", "marginal"),
            ("Large Triangles (3× average edge)", "large"),
            ("Largest Connected Part", "largest"),
            ("Invert Selection", "invert"),
            ("Deselect All", "none"),
        ] {
            selection.append(Some(label), Some(&format!("win.select-triangles::{target}")));
        }
        let selection_apply = gio::Menu::new();
        selection_apply.append(Some("_Remove Selected Triangles"), Some("win.remove-selected"));
        selection.append_section(None, &selection_apply);
        let cut = gio::Menu::new();
        cut.append(Some("Keep Only _Inside the Region"), Some("win.cut-inside"));
        cut.append(Some("Keep Only _Outside the Region"), Some("win.cut-outside"));
        selection.append_section(Some("Cut by region"), &cut);
        tools_menu.append_submenu(Some("Select _Triangles"), &selection);
        let lasso = gio::Menu::new();
        lasso.append(Some("_Lasso Tool"), Some("win.lasso-mode"));
        lasso.append(Some("Remove Lasso-Selected Triangles"), Some("win.lasso-remove"));
        lasso.append(Some("Keep Only Lasso-Selected Triangles"), Some("win.lasso-keep"));
        lasso.append(Some("Invert Lasso Selection"), Some("win.lasso-invert"));
        lasso.append(Some("Clear Lasso Selection"), Some("win.lasso-clear"));
        tools_menu.append_section(Some("Lasso (3D view)"), &lasso);
        let out = gio::Menu::new();
        out.append(Some("_Export Model…"), Some("win.export-model"));
        out.append(Some("Export Sparse _Point Cloud…"), Some("win.export-sparse-cloud"));
        out.append(Some("Export _Registration…"), Some("win.export-registration"));
        out.append(Some("Export _Undistorted Images…"), Some("win.export-undistorted"));
        out.append(Some("Write _XMP Camera Files"), Some("win.export-xmp"));
        out.append(Some("Export Control Point _Measurements…"), Some("win.export-measurements"));
        out.append(Some("Export _Ground Control Points…"), Some("win.export-gcp"));
        out.append(Some("Show in _Viewport"), Some("win.show-mesh"));
        tools_menu.append_submenu(Some("E_xport"), &out);
        let cps = gio::Menu::new();
        cps.append(Some("_Draft Alignment"), Some("win.draft-align"));
        cps.append(Some("Detect _Features"), Some("win.detect-features"));
        cps.append(Some("Detect _Markers"), Some("win.detect-markers"));
        cps.append(Some("Import _Ground Control Points…"), Some("win.import-gcp"));
        cps.append(Some("_Update Alignment to Constraints"), Some("win.update-alignment"));
        tools_menu.append_section(Some("Alignment"), &cps);
        let masks = gio::Menu::new();
        masks.append(Some("AI Masks for _Selected Images"), Some("win.generate-masks-selected"));
        masks.append(Some("Generate _AI Masks (all images)"), Some("win.generate-masks"));
        masks.append(Some("Masks from the Selected _Model"), Some("win.mask-from-mesh"));
        masks.append(Some("_Import Masks from Folder…"), Some("win.import-masks"));
        masks.append(Some("_Export Masks…"), Some("win.export-masks"));
        tools_menu.append_submenu(Some("_Masks"), &masks);
        let comps = gio::Menu::new();
        comps.append(Some("Merge Components"), Some("win.merge-components"));
        comps.append(Some("Export Component…"), Some("win.export-component"));
        comps.append(Some("Import Component…"), Some("win.import-component"));
        tools_menu.append_section(Some("Components"), &comps);
        let tools = gtk::MenuButton::builder()
            .label("Tools")
            .menu_model(&tools_menu)
            .tooltip_text("Mesh tools and export for the selected model")
            .build();
        header.pack_start(&tools);

        let menu = gio::Menu::new();
        let recent_menu = gio::Menu::new();
        menu.append_submenu(Some("Open _Recent"), &recent_menu);
        menu.append(Some("Save _As…"), Some("win.save-as"));
        menu.append(Some("_Export Model…"), Some("win.export-model"));
        menu.append(Some("_Refresh State"), Some("win.refresh"));
        menu.append(Some("Show _Log"), Some("win.toggle-log"));
        menu.append(Some("_Discard Autosaves"), Some("win.discard-autosaves"));
        let runtime_menu = gio::Menu::new();
        runtime_menu.append(Some("Looking for RealityScan…"), None);
        menu.append_submenu(Some("RealityScan R_untime"), &runtime_menu);
        let section = gio::Menu::new();
        section.append(Some("_About RealityScan GTK"), Some("win.about"));
        section.append(Some("_Quit"), Some("win.quit"));
        menu.append_section(None, &section);
        let menu_button = gtk::MenuButton::builder()
            .icon_name("open-menu-symbolic")
            .menu_model(&menu)
            .tooltip_text("Main menu")
            .build();
        header.pack_end(&menu_button);
        let abort = icon_button("process-stop-symbolic", "Abort the running process (Esc)", "win.abort");
        header.pack_end(&abort);
        // The whole workflow, right of the title like RealityScan's Start button.
        let run = gtk::Button::builder()
            .action_name("win.run-all")
            .tooltip_text("Run the whole workflow: align, model at normal detail, texture (F9)")
            .build();
        let run_content =
            adw::ButtonContent::builder().icon_name("media-playback-start-symbolic").label("Run").build();
        run.set_child(Some(&run_content));
        run.add_css_class("suggested-action");
        header.pack_end(&run);

        // ----- sidebar: image grid
        let image_count = gtk::Label::builder().label("No images").xalign(0.0).hexpand(true).build();
        image_count.add_css_class("heading");
        let images_menu = gio::Menu::new();
        images_menu.append(Some("Select _All"), Some("win.select-all-images"));
        images_menu.append(Some("Select _None"), Some("win.deselect-images"));
        let flags = gio::Menu::new();
        for (label, flag) in [("alignment", "align"), ("meshing", "mesh"), ("texturing", "texture")] {
            flags.append(Some(&format!("Use for {label}")), Some(&format!("win.image-flag::{flag}-on")));
            flags.append(Some(&format!("Skip for {label}")), Some(&format!("win.image-flag::{flag}-off")));
        }
        images_menu.append_section(Some("Selected images"), &flags);
        let ai = gio::Menu::new();
        ai.append(Some("Generate AI _Masks"), Some("win.generate-masks-selected"));
        images_menu.append_section(None, &ai);
        let images_button = gtk::MenuButton::builder()
            .icon_name("view-more-symbolic")
            .menu_model(&images_menu)
            .tooltip_text("Image selection and per-image switches")
            .build();
        images_button.add_css_class("flat");
        let sidebar_header = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .margin_start(12)
            .margin_end(6)
            .margin_top(6)
            .margin_bottom(2)
            .build();
        sidebar_header.append(&image_count);
        sidebar_header.append(&images_button);
        let image_grid = gtk::FlowBox::builder()
            .selection_mode(gtk::SelectionMode::Multiple)
            .homogeneous(true)
            .min_children_per_line(2)
            .max_children_per_line(8)
            .column_spacing(6)
            .row_spacing(8)
            .margin_start(12)
            .margin_end(12)
            .margin_bottom(12)
            .valign(gtk::Align::Start)
            .build();
        let grid_scroll = gtk::ScrolledWindow::builder()
            .child(&image_grid)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .build();
        let empty_buttons = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .halign(gtk::Align::Center)
            .build();
        let add_btn = gtk::Button::builder().label("Add Images…").action_name("win.add-images").build();
        add_btn.add_css_class("pill");
        add_btn.add_css_class("suggested-action");
        let folder_btn = gtk::Button::builder().label("Add Folder…").action_name("win.add-folder").build();
        folder_btn.add_css_class("pill");
        empty_buttons.append(&add_btn);
        empty_buttons.append(&folder_btn);
        let empty = adw::StatusPage::builder()
            .icon_name("camera-photo-symbolic")
            .title("No images yet")
            .description("Add photos of your subject, open a project, or drop files here.")
            .child(&empty_buttons)
            .build();
        empty.add_css_class("compact");
        let image_stack = gtk::Stack::builder().vexpand(true).build();
        image_stack.add_named(&empty, Some("empty"));
        image_stack.add_named(&grid_scroll, Some("grid"));
        let images_pane = gtk::Box::new(gtk::Orientation::Vertical, 0);
        images_pane.append(&sidebar_header);
        images_pane.append(&image_stack);

        // ----- sidebar: components and models
        let components_list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::Single).build();
        components_list.add_css_class("navigation-sidebar");
        let models_list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::Single).build();
        models_list.add_css_class("navigation-sidebar");
        let cp_list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::Single).build();
        cp_list.add_css_class("navigation-sidebar");
        let constraint_list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).build();
        constraint_list.add_css_class("navigation-sidebar");
        let scene_box = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(2).build();
        scene_box.append(&section_label("Components"));
        scene_box.append(&components_list);
        scene_box.append(&section_label("Models"));
        scene_box.append(&models_list);
        scene_box.append(&section_header(
            "Control points",
            "win.new-control-point",
            "New control point: name it, then click its position in images",
        ));
        scene_box.append(&cp_list);
        scene_box.append(&section_header(
            "Distances",
            "win.define-distance",
            "Define a known distance between two control points",
        ));
        scene_box.append(&constraint_list);
        let ortho_list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).build();
        ortho_list.add_css_class("navigation-sidebar");
        scene_box.append(&section_header(
            "Ortho projections",
            "win.new-ortho",
            "Render an orthographic view of the selected model through a face of the region",
        ));
        scene_box.append(&ortho_list);
        let scene_scroll = gtk::ScrolledWindow::builder()
            .child(&scene_box)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .min_content_height(120)
            .build();
        let sidebar = gtk::Paned::builder()
            .orientation(gtk::Orientation::Vertical)
            .start_child(&images_pane)
            .end_child(&scene_scroll)
            .position(400)
            .shrink_start_child(false)
            .shrink_end_child(false)
            .build();
        sidebar.add_css_class("sidebar-pane");

        // ----- viewport and image view
        let viewport = Viewport::new();
        let image_view = ImageView::new();
        let view_stack = gtk::Stack::builder().transition_type(gtk::StackTransitionType::Crossfade).build();
        view_stack.add_named(viewport.widget(), Some("3d"));
        view_stack.add_named(image_view.widget(), Some("image"));
        let split = gtk::Paned::builder()
            .orientation(gtk::Orientation::Horizontal)
            .start_child(&sidebar)
            .end_child(&view_stack)
            .position(380)
            .shrink_start_child(false)
            .shrink_end_child(false)
            .build();

        // ----- log (collapsible)
        let log_view = gtk::TextView::builder()
            .editable(false)
            .monospace(true)
            .cursor_visible(false)
            .left_margin(8)
            .right_margin(8)
            .top_margin(4)
            .build();
        log_view.set_wrap_mode(gtk::WrapMode::WordChar);
        let log = log_view.buffer();
        let log_scroll = gtk::ScrolledWindow::builder().child(&log_view).min_content_height(150).build();
        let log_revealer = gtk::Revealer::builder()
            .child(&log_scroll)
            .transition_type(gtk::RevealerTransitionType::SlideUp)
            .reveal_child(false)
            .build();

        // ----- status bar
        let spinner = adw::Spinner::new();
        let status = gtk::Label::builder().label("Starting…").xalign(0.0).build();
        let activity =
            gtk::Label::builder().xalign(0.0).ellipsize(gtk::pango::EllipsizeMode::End).hexpand(true).build();
        activity.add_css_class("dim-label");
        let progress =
            gtk::ProgressBar::builder().width_request(200).valign(gtk::Align::Center).visible(false).build();
        let log_toggle = gtk::ToggleButton::builder()
            .icon_name("view-list-symbolic")
            .tooltip_text("Show the log (Ctrl+L)")
            .action_name("win.toggle-log")
            .build();
        log_toggle.add_css_class("flat");
        let status_bar = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(10)
            .margin_start(12)
            .margin_end(6)
            .margin_top(4)
            .margin_bottom(4)
            .build();
        status_bar.append(&spinner);
        status_bar.append(&status);
        status_bar.append(&activity);
        status_bar.append(&progress);
        status_bar.append(&log_toggle);

        let banner = adw::Banner::builder()
            .title("RealityScan is not running")
            .button_label("Restart RealityScan")
            .revealed(false)
            .build();
        banner.connect_button_clicked(|b| {
            let _ = b.activate_action("win.relaunch", None);
        });

        let autosave_banner = adw::Banner::builder().button_label("Discard").revealed(false).build();

        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.append(&banner);
        content.append(&autosave_banner);
        content.append(&split);
        content.append(&log_revealer);
        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&content));

        let view = adw::ToolbarView::new();
        view.add_top_bar(&header);
        view.set_content(Some(&toasts));
        view.add_bottom_bar(&status_bar);
        window.set_content(Some(&view));

        let actions = gio::SimpleActionGroup::new();
        window.insert_action_group("win", Some(&actions));

        let inner = Rc::new(Inner {
            backend,
            actions,
            title,
            spinner,
            status,
            activity,
            progress,
            image_grid,
            image_stack,
            image_count,
            step_buttons,
            thumbs: Thumbnails::start(),
            pictures: RefCell::new(HashMap::new()),
            cells: RefCell::new(Vec::new()),
            components_list,
            models_list,
            cp_list,
            constraint_list,
            ortho_list,
            orthos: RefCell::new(Vec::new()),
            pending_ortho_view: RefCell::new(None),
            scene_items: RefCell::new(SceneItems::default()),
            cps: RefCell::new(ControlPointData::default()),
            active_point: RefCell::new(None),
            hidden_points: RefCell::new(std::collections::HashSet::new()),
            pending_placement: RefCell::new(None),
            image_view,
            view_stack,
            region_panel,
            align_panel,
            menu_buttons: HashMap::from([
                ("tools", tools.clone()),
                ("menu", menu_button.clone()),
                ("images", images_button.clone()),
            ]),
            toasts,
            recent_menu,
            runtime_menu,
            log,
            log_view,
            banner,
            autosave_banner,
            autosave_file: RefCell::new(None),
            viewport,
            scene: RefCell::new(None),
            cameras: RefCell::new(Vec::new()),
            project_file: RefCell::new(None),
            last_recent: RefCell::new(None),
            open_on_start: RefCell::new(open_on_start),
            connected: Cell::new(false),
            owned: Cell::new(true),
            busy: Cell::new(false),
            has_model: Cell::new(false),
            has_component: Cell::new(false),
            syncing: Cell::new(false),
            stopping: Cell::new(false),
            backend_done: Cell::new(false),
        });
        let this = Self { window, inner };
        this.install_actions(app, &log_revealer);
        if let Some(a) = this.inner.action("discard-autosaves") {
            a.set_enabled(false);
        }
        if let Ok(script) = std::env::var("REALITYSCAN_GTK_SCRIPT") {
            this.run_script(script);
        }
        this.inner.rebuild_recent_menu(recent::load());
        this.inner.region_panel.set_region(None);
        this.inner.update_actions();

        this.window.connect_close_request(glib::clone!(
            #[strong]
            this,
            move |_| this.request_close()
        ));

        this.inner.autosave_banner.connect_button_clicked(|b| {
            let _ = WidgetExt::activate_action(b, "win.discard-autosaves", None);
        });

        this.inner.image_grid.connect_child_activated(glib::clone!(
            #[strong]
            this,
            move |_, child| this.open_image(child.index() as usize)
        ));
        this.inner.image_view.back_button().connect_clicked(glib::clone!(
            #[strong]
            this,
            move |_| this.inner.view_stack.set_visible_child_name("3d")
        ));
        this.inner.image_view.connect_place(glib::clone!(
            #[strong]
            this,
            move |x, y| this.place_measurement(x, y)
        ));
        this.inner.image_view.connect_remove(glib::clone!(
            #[strong]
            this,
            move |point| this.remove_measurement(&point)
        ));
        this.inner.image_view.connect_move(glib::clone!(
            #[strong]
            this,
            move |point, x, y| this.move_measurement(&point, x, y)
        ));
        this.inner.image_view.set_mask_dir(crate::backend::cache_dir("masks"));
        this.inner.image_view.connect_apply_mask(glib::clone!(
            #[strong]
            this,
            move |png| {
                let index = {
                    let scene = this.inner.scene.borrow();
                    let current = this.inner.image_view.current_image();
                    scene
                        .as_ref()
                        .zip(current)
                        .and_then(|(s, rs)| s.images.iter().find(|i| i.wine_path() == rs).map(|i| i.index))
                };
                if let Some(image_index) = index {
                    this.inner.backend.send(Cmd::ApplyMask { image_index, path: png });
                }
            }
        ));
        this.inner.image_view.connect_clear_mask(glib::clone!(
            #[strong]
            this,
            move || {
                if let Some(rs) = this.inner.image_view.current_image() {
                    this.inner.backend.send(Cmd::RemoveMask(rs));
                }
            }
        ));
        this.inner.viewport.connect_camera_activated(glib::clone!(
            #[strong]
            this,
            move |camera| {
                let index = {
                    let scene = this.inner.scene.borrow();
                    let cameras = this.inner.cameras.borrow();
                    cameras
                        .iter()
                        .find(|c| c.index == camera)
                        .and_then(|cam| scene.as_ref().and_then(|s| s.image_position_for_camera(cam)))
                };
                if let Some(i) = index {
                    this.open_image(i);
                }
            }
        ));
        this.inner.cp_list.connect_row_selected(glib::clone!(
            #[strong]
            this,
            move |_, row| {
                if this.inner.syncing.get() {
                    return;
                }
                let name = row.and_then(|r| {
                    this.inner.cps.borrow().points.get(r.index() as usize).map(|p| p.name.clone())
                });
                this.inner.set_active_point(name);
            }
        ));
        this.inner.align_panel.set_values(&crate::align_panel::load_saved());
        this.inner.align_panel.connect_apply(glib::clone!(
            #[strong]
            this,
            move |pairs| {
                crate::align_panel::save(&pairs);
                this.inner.backend.send(Cmd::SetSettings(pairs));
            }
        ));

        // Sidebar interactions
        this.inner.region_panel.connect_apply(glib::clone!(
            #[strong]
            this,
            move |region| this.inner.backend.send(Cmd::SetRegion(region))
        ));
        this.inner.image_grid.connect_selected_children_changed(glib::clone!(
            #[strong]
            this,
            move |_| this.inner.sync_camera_selection()
        ));
        this.inner.viewport.connect_camera_clicked(glib::clone!(
            #[strong]
            this,
            move |camera| this.inner.select_image_for_camera(camera)
        ));
        this.inner.viewport.connect_point_picked(glib::clone!(
            #[strong]
            this,
            move |position, normal| this.place_point_on_model(position, normal)
        ));
        this.inner.viewport.connect_cameras_lassoed(glib::clone!(
            #[strong]
            this,
            move |cameras, mode| this.inner.select_images_for_cameras(&cameras, mode)
        ));
        this.inner.viewport.connect_edit(glib::clone!(
            #[strong]
            this,
            move |mesh, keep| this.inner.backend.send(Cmd::ImportMesh { mesh: EditMesh(mesh), keep })
        ));
        this.inner.viewport.connect_selection_changed(glib::clone!(
            #[strong]
            this,
            move |count| {
                for name in ["lasso-remove", "lasso-keep", "lasso-invert", "lasso-clear"] {
                    if let Some(a) = this.inner.action(name) {
                        a.set_enabled(count > 0);
                    }
                }
            }
        ));
        this.inner.viewport.connect_region_changed(glib::clone!(
            #[strong]
            this,
            move |region| {
                this.inner.region_panel.set_region(Some(&region));
                this.inner.backend.send(Cmd::SetRegion(region));
            }
        ));
        this.inner.components_list.connect_row_selected(glib::clone!(
            #[strong]
            this,
            move |_, row| {
                if this.inner.syncing.get() {
                    return;
                }
                let Some(row) = row else { return };
                let items = this.inner.scene_items.borrow();
                let Some((guid, name)) = items.components.get(row.index() as usize) else { return };
                let already = this
                    .inner
                    .scene
                    .borrow()
                    .as_ref()
                    .and_then(|s| s.project.selected_component().map(str::to_owned));
                if already.as_deref() != Some(guid.as_str()) {
                    this.inner.backend.send(Cmd::SelectComponent(name.clone()));
                }
            }
        ));
        this.inner.models_list.connect_row_selected(glib::clone!(
            #[strong]
            this,
            move |_, row| {
                if this.inner.syncing.get() {
                    return;
                }
                let Some(row) = row else { return };
                let items = this.inner.scene_items.borrow();
                let Some((guid, name, _)) = items.models.get(row.index() as usize) else { return };
                let already = this
                    .inner
                    .scene
                    .borrow()
                    .as_ref()
                    .and_then(|s| s.project.selected_model().map(str::to_owned));
                if already.as_deref() != Some(guid.as_str()) {
                    this.inner.backend.send(Cmd::SelectModel(name.clone()));
                }
            }
        ));

        // Drag and drop of images, folders and projects onto the window.
        let drop_target = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
        drop_target.connect_drop(glib::clone!(
            #[strong]
            this,
            move |_, value, _, _| {
                let Ok(list) = value.get::<gdk::FileList>() else { return false };
                this.add_dropped(list.files().iter().filter_map(|f| f.path()).collect());
                true
            }
        ));
        this.window.add_controller(drop_target);

        // Backend messages
        glib::spawn_future_local(glib::clone!(
            #[strong]
            this,
            async move {
                let rx = this.inner.backend.msgs.clone();
                while let Ok(msg) = rx.recv().await {
                    let stopped = matches!(msg, Msg::Stopped);
                    this.handle(msg);
                    if stopped {
                        break;
                    }
                }
                if this.inner.stopping.get() {
                    this.window.destroy();
                }
            }
        ));

        // Thumbnails
        glib::spawn_future_local(glib::clone!(
            #[strong]
            this,
            async move {
                let rx = this.inner.thumbs.results.clone();
                while let Ok((path, result)) = rx.recv().await {
                    let pictures = this.inner.pictures.borrow();
                    match (pictures.get(&path), result) {
                        (Some(picture), Ok(thumb)) => picture.set_filename(Some(&thumb)),
                        (Some(_), Err(e)) => tracing::warn!("thumbnail for {}: {e}", path.display()),
                        (None, _) => {}
                    }
                    drop(pictures);
                    // Keep the header's "to go" count honest.
                    let label = this.inner.image_count.label().to_string();
                    let base = label.split(" · ").take(2).collect::<Vec<_>>().join(" · ");
                    let pending = this.inner.thumbs.pending();
                    this.inner.image_count.set_label(&if pending > 0 {
                        format!("{base} · {pending} thumbnails to go")
                    } else {
                        base
                    });
                }
            }
        ));
        this
    }

    pub fn present(&self) {
        self.window.present();
    }

    fn install_actions(&self, app: &adw::Application, log_revealer: &gtk::Revealer) {
        let add = |name: &str, accels: &[&str], f: Box<dyn Fn(&MainWindow)>| {
            let action = gio::SimpleAction::new(name, None);
            let this = self.clone();
            action.connect_activate(move |_, _| f(&this));
            self.inner.actions.add_action(&action);
            if !accels.is_empty() {
                app.set_accels_for_action(&format!("win.{name}"), accels);
            }
        };
        type StrHandler = Box<dyn Fn(&MainWindow, &str)>;
        let add_str = |name: &str, f: StrHandler| {
            let action = gio::SimpleAction::new(name, Some(glib::VariantTy::STRING));
            let this = self.clone();
            action.connect_activate(move |_, param| {
                if let Some(s) = param.and_then(|p| p.get::<String>()) {
                    f(&this, &s);
                }
            });
            self.inner.actions.add_action(&action);
        };
        add("open", &["<Control>o"], Box::new(|w| w.pick_project_to_open()));
        add_str("open-recent", Box::new(|w, path| w.inner.backend.send(Cmd::Open(PathBuf::from(path)))));
        add(
            "save",
            &["<Control>s"],
            Box::new(|w| {
                if w.inner.project_file.borrow().is_some() {
                    w.inner.backend.send(Cmd::Save(None));
                } else {
                    w.pick_project_to_save();
                }
            }),
        );
        add("save-as", &["<Control><Shift>s"], Box::new(|w| w.pick_project_to_save()));
        add("add-images", &["<Control>i"], Box::new(|w| w.pick_images()));
        add("add-folder", &["<Control><Shift>i"], Box::new(|w| w.pick_folder()));
        add("select-all-images", &["<Control>a"], Box::new(|w| w.inner.image_grid.select_all()));
        add("deselect-images", &["<Control><Shift>a"], Box::new(|w| w.inner.image_grid.unselect_all()));
        add_str("image-flag", Box::new(|w, which| w.set_image_flag(which)));
        add("align", &[], Box::new(|w| w.inner.backend.send(Cmd::Align)));
        add("run-all", &["F9"], Box::new(|w| w.inner.backend.send(Cmd::RunAll)));
        add(
            "align-with-settings",
            &[],
            Box::new(|w| {
                w.inner.align_panel.apply();
                w.inner.align_panel.popover().popdown();
                w.inner.backend.send(Cmd::Align);
            }),
        );
        add("model", &[], Box::new(|w| w.inner.backend.send(Cmd::Model(Detail::Normal))));
        add("model-preview", &[], Box::new(|w| w.inner.backend.send(Cmd::Model(Detail::Preview))));
        add("model-high", &[], Box::new(|w| w.inner.backend.send(Cmd::Model(Detail::High))));
        add("colorize", &[], Box::new(|w| w.inner.backend.send(Cmd::Colorize)));
        add("texture", &[], Box::new(|w| w.inner.backend.send(Cmd::Texture)));
        add("show-mesh", &["<Control>m"], Box::new(|w| w.inner.backend.send(Cmd::ShowMesh)));
        add("simplify", &[], Box::new(|w| w.prompt_simplify()));
        add("smooth", &[], Box::new(|w| w.inner.backend.send(Cmd::Smooth)));
        add("close-holes", &[], Box::new(|w| w.inner.backend.send(Cmd::CloseHoles)));
        add("clean-model", &[], Box::new(|w| w.inner.backend.send(Cmd::CleanModel)));
        add("keep-largest", &[], Box::new(|w| w.inner.backend.send(Cmd::KeepLargestPart)));
        add(
            "export-model",
            &["<Control>e"],
            Box::new(|w| {
                if let Some((_, name, textured)) = w.inner.selected_model() {
                    w.export_model(&name, textured);
                }
            }),
        );
        add("draft-align", &[], Box::new(|w| w.inner.backend.send(Cmd::DraftAlign)));
        add("detect-features", &[], Box::new(|w| w.inner.backend.send(Cmd::DetectFeatures)));
        add("generate-masks", &[], Box::new(|w| w.inner.backend.send(Cmd::GenerateMasks)));
        add("remove-selected", &[], Box::new(|w| w.inner.backend.send(Cmd::RemoveSelectedTriangles)));
        add(
            "lasso-mode",
            &[],
            Box::new(|w| {
                let on = !w.inner.viewport.lasso_mode();
                w.inner.viewport.set_lasso_mode(on);
            }),
        );
        add("lasso-remove", &["Delete"], Box::new(|w| w.inner.viewport.edit_selection(false)));
        add("lasso-keep", &[], Box::new(|w| w.inner.viewport.edit_selection(true)));
        add("lasso-invert", &[], Box::new(|w| w.inner.viewport.invert_selection()));
        add("lasso-clear", &[], Box::new(|w| w.inner.viewport.clear_selection()));
        for name in ["lasso-remove", "lasso-keep", "lasso-invert", "lasso-clear"] {
            if let Some(a) = self.inner.action(name) {
                a.set_enabled(false);
            }
        }
        add(
            "cut-inside",
            &[],
            Box::new(|w| w.inner.backend.send(Cmd::CutByRegion { keep_inside: true, fill_holes: false })),
        );
        add(
            "cut-outside",
            &[],
            Box::new(|w| w.inner.backend.send(Cmd::CutByRegion { keep_inside: false, fill_holes: false })),
        );
        add("fill-holes", &[], Box::new(|w| w.inner.backend.send(Cmd::FillHoles)));
        add("export-xmp", &[], Box::new(|w| w.inner.backend.send(Cmd::ExportXmp)));
        add_str(
            "select-triangles",
            Box::new(|w, which| {
                let selector = match which {
                    "inside" => Selector::InsideRegion,
                    "outside" => Selector::OutsideRegion,
                    "marginal" => Selector::Marginal,
                    "large" => Selector::LargeRel(3.0),
                    "largest" => Selector::LargestPart,
                    "invert" => Selector::Invert,
                    _ => Selector::None,
                };
                w.inner.backend.send(Cmd::SelectTriangles(selector));
            }),
        );
        add_str(
            "texture-op",
            Box::new(|w, which| {
                let op = match which {
                    "unwrap" => TextureOp::Unwrap,
                    "quality-texture" => TextureOp::QualityTexture,
                    "correct-colors" => TextureOp::CorrectColors,
                    "preview-colors" => TextureOp::PreviewColors,
                    _ => TextureOp::QualityColors,
                };
                w.inner.backend.send(Cmd::TextureOp(op));
            }),
        );
        add(
            "import-gcp",
            &[],
            Box::new(|w| {
                w.pick_file_to_import(
                    "Import ground control points",
                    &["csv", "txt"],
                    Cmd::ImportGroundControl,
                )
            }),
        );
        add(
            "export-gcp",
            &[],
            Box::new(|w| {
                w.pick_file_to_export(
                    "Export ground control points",
                    "ground-control.csv",
                    &["csv"],
                    Cmd::ExportGroundControl,
                )
            }),
        );
        add(
            "export-measurements",
            &[],
            Box::new(|w| {
                w.pick_file_to_export(
                    "Export control point measurements",
                    "measurements.csv",
                    &["csv"],
                    Cmd::ExportMeasurements,
                )
            }),
        );
        add(
            "export-sparse-cloud",
            &[],
            Box::new(|w| {
                w.pick_file_to_export(
                    "Export sparse point cloud",
                    "sparse.ply",
                    &["ply", "xyz", "las", "ptx", "obj"],
                    Cmd::ExportSparseCloud,
                )
            }),
        );
        add(
            "export-registration",
            &[],
            Box::new(|w| {
                w.pick_file_to_export(
                    "Export registration",
                    "cameras.csv",
                    &["csv", "txt", "xml", "out"],
                    Cmd::ExportRegistration,
                )
            }),
        );
        add(
            "export-undistorted",
            &[],
            Box::new(|w| w.pick_folder_for("Export undistorted images into", Cmd::ExportUndistorted)),
        );
        add("mask-from-mesh", &[], Box::new(|w| w.inner.backend.send(Cmd::MaskFromMesh)));
        add(
            "generate-masks-selected",
            &[],
            Box::new(|w| {
                let images = w.inner.selected_image_paths();
                if images.is_empty() {
                    w.inner.toasts.add_toast(adw::Toast::new("Select some images first"));
                } else {
                    w.inner.backend.send(Cmd::GenerateMasksFor(images));
                }
            }),
        );
        add(
            "generate-mask-current",
            &[],
            Box::new(|w| {
                if let Some(rs) = w.inner.image_view.current_image() {
                    w.inner.backend.send(Cmd::GenerateMasksFor(vec![rs]));
                }
            }),
        );
        add(
            "import-masks",
            &[],
            Box::new(|w| {
                w.pick_folder_for("Folder with mask images named like the photos", Cmd::ImportMasks)
            }),
        );
        add("export-masks", &[], Box::new(|w| w.pick_folder_for("Export masks into", Cmd::ExportMasks)));
        add("new-ortho", &[], Box::new(|w| w.prompt_ortho()));
        add("new-control-point", &[], Box::new(|w| w.prompt_new_control_point()));
        add("define-distance", &[], Box::new(|w| w.prompt_distance()));
        add("detect-markers", &[], Box::new(|w| w.inner.backend.send(Cmd::DetectMarkers)));
        add("update-alignment", &[], Box::new(|w| w.inner.backend.send(Cmd::UpdateAlignment)));
        add("merge-components", &[], Box::new(|w| w.inner.backend.send(Cmd::MergeComponents)));
        add("export-component", &[], Box::new(|w| w.pick_component_export()));
        add("import-component", &[], Box::new(|w| w.pick_component_import()));
        add("region-auto", &[], Box::new(|w| w.inner.backend.send(Cmd::RegionAuto)));
        add("region-density", &[], Box::new(|w| w.inner.backend.send(Cmd::RegionByDensity)));
        add("abort", &["Escape"], Box::new(|w| w.inner.backend.send(Cmd::Abort)));
        add("refresh", &["F5"], Box::new(|w| w.inner.backend.send(Cmd::Refresh)));
        add(
            "relaunch",
            &["<Control>r"],
            Box::new(|w| {
                w.inner.banner.set_revealed(false);
                w.inner.backend.send(Cmd::Relaunch);
            }),
        );
        add(
            "discard-autosaves",
            &[],
            Box::new(|w| {
                w.inner.autosave_banner.set_revealed(false);
                w.inner.autosave_file.borrow_mut().take();
                w.inner.backend.send(Cmd::DiscardAutosaves);
                if let Some(a) = w.inner.action("discard-autosaves") {
                    a.set_enabled(false);
                }
            }),
        );
        add("about", &[], Box::new(|w| w.show_about()));
        add("quit", &["<Control>q"], Box::new(|w| w.window.close()));

        let toggle_log = gio::SimpleAction::new_stateful("toggle-log", None, &false.to_variant());
        let revealer = log_revealer.clone();
        toggle_log.connect_activate(move |action, _| {
            let shown = !action.state().and_then(|s| s.get::<bool>()).unwrap_or(false);
            action.set_state(&shown.to_variant());
            revealer.set_reveal_child(shown);
        });
        self.inner.actions.add_action(&toggle_log);
        app.set_accels_for_action("win.toggle-log", &["<Control>l"]);

        // Which RealityScan to run; the items come from the backend's detection.
        let runtime =
            gio::SimpleAction::new_stateful("runtime", Some(glib::VariantTy::STRING), &"".to_variant());
        let this = self.clone();
        runtime.connect_activate(move |action, param| {
            let Some(spec) = param.and_then(|p| p.get::<String>()) else { return };
            let current = action.state().and_then(|s| s.get::<String>()).unwrap_or_default();
            if spec == current {
                return;
            }
            action.set_state(&spec.to_variant());
            this.inner.backend.send(Cmd::SetRuntime(spec));
        });
        self.inner.actions.add_action(&runtime);
    }

    fn handle(&self, msg: Msg) {
        let inner = &self.inner;
        match msg {
            Msg::Starting(text) => {
                inner.title.set_subtitle(&text);
                inner.status.set_label(&text);
                inner.spinner.set_visible(true);
                inner.log_line(&text);
            }
            Msg::Ready { version, owned, runtime } => {
                inner.owned.set(owned);
                inner.banner.set_revealed(false);
                if let Some(a) = inner.action("relaunch") {
                    a.set_enabled(false);
                }
                inner.title.set_subtitle(&if runtime.is_empty() {
                    format!("RealityScan {version}")
                } else {
                    format!("RealityScan {version} · {runtime}")
                });
                inner.status.set_label("Idle");
                inner.spinner.set_visible(false);
                inner.set_connected(true);
                inner.log_line("Connected");
                if let Some(path) = inner.open_on_start.borrow_mut().take() {
                    inner.backend.send(Cmd::Open(path));
                }
            }
            Msg::Runtimes { available, current } => {
                inner.runtime_menu.remove_all();
                if available.is_empty() {
                    inner.runtime_menu.append(Some("No RealityScan install found"), None);
                }
                for (spec, label) in &available {
                    let item = gio::MenuItem::new(Some(label), None);
                    item.set_action_and_target_value(Some("win.runtime"), Some(&spec.to_variant()));
                    inner.runtime_menu.append_item(&item);
                }
                if let Some(a) = inner.action("runtime") {
                    a.set_state(&current.unwrap_or_default().to_variant());
                }
            }
            Msg::Failed(text) => {
                inner.title.set_subtitle("Not connected");
                inner.status.set_label("Not connected");
                inner.spinner.set_visible(false);
                inner.progress.set_visible(false);
                inner.set_busy(false);
                inner.set_connected(false);
                inner.log_line(&format!("ERROR: {text}"));
                inner.show_log();
            }
            Msg::Status(status) => {
                let busy = !status.is_idle();
                inner.set_busy(busy);
                inner.spinner.set_visible(busy);
                inner.progress.set_visible(busy);
                if busy {
                    inner.progress.set_fraction(status.progress.clamp(0.0, 1.0));
                    let mut text = format!("Working… {:.0}%", status.progress * 100.0);
                    if status.time_estimation > 0.0 {
                        // RealityScan's own guess; it's unreliable early in a process.
                        let mins = (status.time_estimation / 60.0).ceil() as i64;
                        text.push_str(&format!(
                            " · about {}",
                            if mins <= 1 { "a minute".to_owned() } else { format!("{mins} min") }
                        ));
                    }
                    inner.status.set_label(&text);
                } else {
                    inner.activity.set_label("");
                    inner.status.set_label(if status.error_code == 0 {
                        "Idle"
                    } else {
                        "Idle · last command failed"
                    });
                }
            }
            Msg::Scene(scene) => {
                *inner.project_file.borrow_mut() = (!scene.project.path.is_empty()).then(|| {
                    format!("{}.rsproj", scene.project.path.rsplit(['\\', '/']).next().unwrap_or("project"))
                });
                inner.remember_project(&scene);
                inner.show_scene(&scene);
                self.show_lists(&scene);
                *inner.scene.borrow_mut() = Some(scene);
                inner.update_actions();
            }
            Msg::Sparse { cloud, cameras } => {
                if cloud.is_empty() && cameras.is_empty() {
                    inner.viewport.clear();
                } else {
                    inner.viewport.set_scene(&cloud, &cameras);
                    inner.log_line(&format!(
                        "Sparse cloud: {} points, {} cameras",
                        cloud.len(),
                        cameras.len()
                    ));
                }
                *inner.cameras.borrow_mut() = cameras;
                inner.sync_camera_selection();
            }
            Msg::Mesh(Some(mesh)) => {
                inner.log_line(&format!(
                    "Mesh: {} triangles{}",
                    mesh.triangle_count(),
                    if mesh.is_textured() {
                        format!(", {} texture(s)", mesh.textures.len())
                    } else {
                        String::new()
                    }
                ));
                inner.viewport.set_mesh(mesh);
            }
            Msg::Mesh(None) => inner.viewport.clear_mesh(),
            Msg::Region(region) => {
                inner.viewport.set_region(region.as_ref());
                inner.region_panel.set_region(region.as_ref());
            }
            Msg::ControlPoints(data) => self.show_control_points(data),
            Msg::Orthos(list) => self.show_orthos(list),
            Msg::Autosaves(list) => {
                // Loading an `.rsproj.autosave` through the API crashes RealityScan, so recovery
                // has to happen in its own window; here the user can only be told and discard.
                if let Some(a) = list.iter().max_by_key(|a| a.modified) {
                    let when = a
                        .modified
                        .map(|t| {
                            glib::DateTime::from_unix_local(
                                t.duration_since(std::time::UNIX_EPOCH)
                                    .map(|d| d.as_secs() as i64)
                                    .unwrap_or(0),
                            )
                        })
                        .and_then(|r| r.ok())
                        .and_then(|d| d.format("%d %b %H:%M").ok())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| "earlier".to_owned());
                    inner.autosave_banner.set_title(&format!(
                        "RealityScan left an autosave from {when}; recover it from RealityScan's own window (Heroic), or discard it"
                    ));
                    *inner.autosave_file.borrow_mut() = a.project_file.clone();
                    inner.autosave_banner.set_revealed(true);
                }
                if let Some(a) = inner.action("discard-autosaves") {
                    a.set_enabled(true);
                }
            }
            Msg::Exported(path) => {
                inner.log_line(&format!("Exported to {}", path.display()));
                let wanted = inner.pending_ortho_view.borrow().clone();
                if wanted.is_some_and(|w| w.with_extension("") == path.with_extension("")) {
                    inner.pending_ortho_view.borrow_mut().take();
                    let name = path.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    inner.image_view.show(&path, "", &name, None, None);
                    inner.view_stack.set_visible_child_name("image");
                    return;
                }
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let toast = adw::Toast::builder()
                    .title(format!("Exported {name}"))
                    .button_label("Show")
                    .timeout(8)
                    .build();
                let window = self.window.clone();
                toast.connect_button_clicked(move |_| {
                    let file = gio::File::for_path(&path);
                    gtk::FileLauncher::new(Some(&file)).open_containing_folder(
                        Some(&window),
                        gio::Cancellable::NONE,
                        |_| {},
                    );
                });
                inner.toasts.add_toast(toast);
            }
            Msg::Event(event) => match event {
                Event::SequenceState { state, message: Some(m), .. } => {
                    inner.log_line(&format!("{state:?}: {m}"));
                    inner.show_log();
                }
                Event::Tag(tag) => inner.log_line(&format!("Reached {tag}")),
                Event::SectionCompleted { algorithm_id, error: Some(e), .. } => {
                    inner.log_line(&format!("Process {algorithm_id} failed: {e}"));
                    inner.show_log();
                }
                _ => {}
            },
            Msg::Activity(line) => {
                if inner.busy.get() {
                    inner.activity.set_label(&line);
                }
            }
            Msg::Log(text) => inner.log_line(&text),
            Msg::Disconnected => {
                inner.title.set_subtitle("Not connected");
                inner.status.set_label("Not connected");
                inner.spinner.set_visible(false);
                inner.progress.set_visible(false);
                inner.set_busy(false);
                inner.set_connected(false);
                inner.banner.set_revealed(true);
                if let Some(a) = inner.action("relaunch") {
                    a.set_enabled(true);
                }
            }
            Msg::Stopped => {
                inner.backend_done.set(true);
                inner.set_connected(false);
                inner.spinner.set_visible(false);
                inner.status.set_label("RealityScan stopped");
                inner.log_line("RealityScan stopped");
            }
        }
    }

    /// Files dropped on the window: projects open, folders and images are added.
    fn add_dropped(&self, paths: Vec<PathBuf>) {
        let mut images = Vec::new();
        for path in paths {
            if path.is_dir() {
                self.inner.backend.send(Cmd::AddFolder(path));
            } else if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("rsproj")) {
                self.inner.backend.send(Cmd::Open(path));
            } else if path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| IMAGE_EXTENSIONS.iter().any(|x| x.eq_ignore_ascii_case(e)))
            {
                images.push(path);
            } else {
                self.inner.log_line(&format!("Ignoring {} (not an image)", path.display()));
            }
        }
        if !images.is_empty() {
            self.inner.backend.send(Cmd::AddImages(images));
        }
    }

    /// `which` is `<align|mesh|texture>-<on|off>`, applied to the selected images.
    fn set_image_flag(&self, which: &str) {
        let Some((flag, state)) = which.split_once('-') else { return };
        let flag = match flag {
            "align" => ImageFlag::Alignment,
            "mesh" => ImageFlag::Meshing,
            "texture" => ImageFlag::Texturing,
            _ => return,
        };
        let images = self.inner.selected_image_paths();
        if images.is_empty() {
            self.inner.toasts.add_toast(adw::Toast::new("Select some images first"));
            return;
        }
        self.inner.backend.send(Cmd::SetImageFlag { images, flag, enabled: state == "on" });
    }

    fn prompt_simplify(&self) {
        let Some((_, name, _)) = self.inner.selected_model() else { return };
        let current = self
            .inner
            .scene
            .borrow()
            .as_ref()
            .and_then(|s| {
                s.components
                    .iter()
                    .flat_map(|c| c.models.iter())
                    .find(|m| m.name == name)
                    .map(|m| m.triangles)
            })
            .unwrap_or(1_000_000);
        // Target as an absolute triangle count or as a percentage of the current count.
        let spin = gtk::SpinButton::with_range(1000.0, 1e9, 100_000.0);
        spin.set_value((current / 2).max(1000) as f64);
        spin.set_digits(0);
        spin.set_hexpand(true);
        let unit = gtk::DropDown::from_strings(&["triangles", "% of current"]);
        unit.connect_selected_notify({
            let spin = spin.clone();
            move |unit| {
                if unit.selected() == 1 {
                    spin.set_range(1.0, 100.0);
                    spin.set_increments(5.0, 25.0);
                    spin.set_value(50.0);
                } else {
                    spin.set_range(1000.0, 1e9);
                    spin.set_increments(100_000.0, 1_000_000.0);
                    spin.set_value((current / 2).max(1000) as f64);
                }
            }
        });
        let row = gtk::Box::builder().orientation(gtk::Orientation::Horizontal).spacing(6).build();
        row.append(&spin);
        row.append(&unit);
        let dialog = adw::AlertDialog::builder()
            .heading(format!("Simplify {name}"))
            .body(format!("The model has {}; reduce it to about:", count(current, "triangle")))
            .extra_child(&row)
            .close_response("cancel")
            .default_response("ok")
            .build();
        dialog.add_response("cancel", "_Cancel");
        dialog.add_response("ok", "_Simplify");
        dialog.set_response_appearance("ok", adw::ResponseAppearance::Suggested);
        let this = self.clone();
        dialog.connect_response(Some("ok"), move |_, _| {
            let target = if unit.selected() == 1 {
                ((current as f64 * spin.value() / 100.0).round() as u64).max(1000)
            } else {
                spin.value() as u64
            };
            this.inner.backend.send(Cmd::Simplify(target));
        });
        dialog.present(Some(&self.window));
    }

    /// Ask for a new name; `on_ok` receives it.
    fn prompt_name(&self, heading: &str, current: &str, on_ok: impl Fn(String) + 'static) {
        let entry = gtk::Entry::builder().text(current).activates_default(true).build();
        let dialog = adw::AlertDialog::builder()
            .heading(heading)
            .extra_child(&entry)
            .close_response("cancel")
            .default_response("ok")
            .build();
        dialog.add_response("cancel", "_Cancel");
        dialog.add_response("ok", "_Rename");
        dialog.set_response_appearance("ok", adw::ResponseAppearance::Suggested);
        let field = entry.clone();
        dialog.connect_response(Some("ok"), move |_, _| {
            let name = field.text().trim().to_owned();
            if !name.is_empty() {
                on_ok(name);
            }
        });
        dialog.present(Some(&self.window));
        entry.grab_focus();
    }

    fn confirm(&self, heading: &str, body: &str, verb: &str, on_ok: impl Fn() + 'static) {
        let dialog = adw::AlertDialog::builder().heading(heading).body(body).close_response("cancel").build();
        dialog.add_response("cancel", "_Cancel");
        dialog.add_response("ok", verb);
        dialog.set_response_appearance("ok", adw::ResponseAppearance::Destructive);
        dialog.connect_response(Some("ok"), move |_, _| on_ok());
        dialog.present(Some(&self.window));
    }

    fn export_model(&self, name: &str, textured: bool) {
        let backend = self.inner.backend.cmds.clone();
        let model = name.to_owned();
        export_dialog::run(&self.window, name, textured, move |path, options| {
            let _ = backend.send_blocking(Cmd::ExportModel { model: model.clone(), path, options });
        });
    }

    fn show_about(&self) {
        let about = adw::AboutDialog::builder()
            .application_name("RealityScan GTK")
            .application_icon("org.realityscan_gtk.RealityScanGtk")
            .version(env!("CARGO_PKG_VERSION"))
            .developer_name("Peter Falkingham")
            .comments(
                "A native Linux front-end for Epic's RealityScan. RealityScan itself runs headless \
                 under Wine and does all the processing; this app drives it through its Remote Command plugin.",
            )
            .build();
        about.present(Some(&self.window));
    }

    fn image_filters() -> gio::ListStore {
        let filter = gtk::FileFilter::new();
        filter.set_name(Some("Images"));
        for ext in IMAGE_EXTENSIONS {
            filter.add_suffix(ext);
        }
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);
        filters
    }

    fn project_filters() -> gio::ListStore {
        let filter = gtk::FileFilter::new();
        filter.set_name(Some("RealityScan projects"));
        filter.add_suffix("rsproj");
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);
        filters
    }

    fn pick_images(&self) {
        let dialog = gtk::FileDialog::builder()
            .title("Add images")
            .filters(&Self::image_filters())
            .modal(true)
            .build();
        let this = self.clone();
        glib::spawn_future_local(async move {
            if let Ok(files) = dialog.open_multiple_future(Some(&this.window)).await {
                let paths: Vec<_> = files.iter::<gio::File>().flatten().filter_map(|f| f.path()).collect();
                if !paths.is_empty() {
                    this.inner.backend.send(Cmd::AddImages(paths));
                }
            }
        });
    }

    fn pick_folder(&self) {
        let dialog = gtk::FileDialog::builder().title("Add a folder of images").modal(true).build();
        let this = self.clone();
        glib::spawn_future_local(async move {
            if let Ok(folder) = dialog.select_folder_future(Some(&this.window)).await
                && let Some(path) = folder.path()
            {
                this.inner.backend.send(Cmd::AddFolder(path));
            }
        });
    }

    fn pick_project_to_open(&self) {
        let dialog = gtk::FileDialog::builder()
            .title("Open project")
            .filters(&Self::project_filters())
            .modal(true)
            .build();
        let this = self.clone();
        glib::spawn_future_local(async move {
            if let Ok(file) = dialog.open_future(Some(&this.window)).await
                && let Some(path) = file.path()
            {
                this.inner.backend.send(Cmd::Open(path));
            }
        });
    }

    fn pick_project_to_save(&self) {
        let dialog = gtk::FileDialog::builder()
            .title("Save project as")
            .filters(&Self::project_filters())
            .initial_name(
                self.inner.project_file.borrow().clone().unwrap_or_else(|| "project.rsproj".to_owned()),
            )
            .modal(true)
            .build();
        let this = self.clone();
        glib::spawn_future_local(async move {
            if let Ok(file) = dialog.save_future(Some(&this.window)).await
                && let Some(mut path) = file.path()
            {
                if path.extension().is_none_or(|e| e != "rsproj") {
                    path.set_extension("rsproj");
                }
                this.inner.backend.send(Cmd::Save(Some(path)));
            }
        });
    }

    /// Ask the backend to quit RealityScan first; the window closes once it has.
    /// While RealityScan is working, ask first: quitting throws the unsaved work away.
    fn request_close(&self) -> glib::Propagation {
        if self.inner.stopping.get() || self.inner.backend_done.get() {
            return glib::Propagation::Proceed;
        }
        if self.inner.busy.get() && self.inner.owned.get() {
            let dialog = adw::AlertDialog::new(
                Some("RealityScan is still working"),
                Some("Quitting now abandons the running process and any unsaved results."),
            );
            dialog.add_responses(&[("cancel", "Keep Working"), ("quit", "Quit Anyway")]);
            dialog.set_response_appearance("quit", adw::ResponseAppearance::Destructive);
            dialog.set_default_response(Some("cancel"));
            dialog.set_close_response("cancel");
            let this = self.clone();
            glib::spawn_future_local(async move {
                if dialog.choose_future(Some(&this.window)).await == "quit" {
                    this.inner.busy.set(false);
                    this.window.close();
                }
            });
            return glib::Propagation::Stop;
        }
        self.inner.stopping.set(true);
        self.inner.set_connected(false);
        self.inner.title.set_subtitle("Shutting down…");
        self.inner.status.set_label("Shutting RealityScan down…");
        self.inner.spinner.set_visible(true);
        self.inner.backend.send(Cmd::Quit);
        glib::Propagation::Stop
    }

    /// Rebuild the component and model lists when their contents changed, and mirror the
    /// selection RealityScan reports.
    fn show_lists(&self, scene: &Scene) {
        let inner = &self.inner;
        let components: Vec<(String, String)> =
            scene.components.iter().map(|c| (c.guid.clone(), c.name.clone())).collect();
        let models: Vec<(String, String, bool)> = scene
            .components
            .iter()
            .flat_map(|c| c.models.iter())
            .map(|m| (m.guid.clone(), m.name.clone(), m.is_textured()))
            .collect();
        let mut items = inner.scene_items.borrow_mut();
        let changed = items.components != components || items.models != models;
        inner.syncing.set(true);
        if changed {
            inner.components_list.remove_all();
            for c in &scene.components {
                let row = adw::ActionRow::builder()
                    .title(&c.name)
                    .subtitle(format!(
                        "{}, {}",
                        count(c.camera_count as u64, "camera"),
                        count(c.models.len() as u64, "model")
                    ))
                    .build();
                row.add_suffix(&self.row_menu(RowKind::Component, &c.guid, &c.name, false));
                inner.components_list.append(&row);
            }
            inner.models_list.remove_all();
            items.model_menus.clear();
            for m in scene.components.iter().flat_map(|c| c.models.iter()) {
                let mut subtitle = count(m.triangles, "triangle");
                if m.is_colored() {
                    subtitle.push_str(" · coloured");
                }
                if let Some(info) = &m.info
                    && info.textured
                {
                    subtitle.push_str(&format!(
                        " · textured {}×{}{}",
                        info.texture_width,
                        info.texture_height,
                        if info.texture_count > 1 {
                            format!(" ×{}", info.texture_count)
                        } else {
                            String::new()
                        }
                    ));
                }
                let row = adw::ExpanderRow::builder().title(&m.name).subtitle(subtitle).build();
                let menu = self.row_menu(RowKind::Model, &m.guid, &m.name, m.is_textured());
                row.add_suffix(&menu);
                items.model_menus.push(menu);
                // What the 3D view draws for this model, RealityScan-tree style.
                if m.is_colored() {
                    let colours = adw::SwitchRow::builder()
                        .title("Vertex colours")
                        .active(inner.viewport.show_colors())
                        .build();
                    let viewport = inner.viewport.clone();
                    colours.connect_active_notify(move |r| viewport.set_show_colors(r.is_active()));
                    row.add_row(&colours);
                }
                if m.is_textured() {
                    let texture = adw::SwitchRow::builder()
                        .title("Texture")
                        .active(inner.viewport.show_textures())
                        .build();
                    let viewport = inner.viewport.clone();
                    texture.connect_active_notify(move |r| viewport.set_show_textures(r.is_active()));
                    row.add_row(&texture);
                }
                if !m.is_colored() && !m.is_textured() {
                    row.set_enable_expansion(false);
                }
                inner.models_list.append(&row);
            }
            items.components = components;
            items.models = models;
        }
        // Mirror RealityScan's selection.
        let sel_c = scene
            .project
            .selected_component()
            .and_then(|g| items.components.iter().position(|(guid, _)| guid == g));
        let sel_m = scene
            .project
            .selected_model()
            .and_then(|g| items.models.iter().position(|(guid, _, _)| guid == g));
        match sel_c.and_then(|i| inner.components_list.row_at_index(i as i32)) {
            Some(row) => inner.components_list.select_row(Some(&row)),
            None => inner.components_list.unselect_all(),
        }
        match sel_m.and_then(|i| inner.models_list.row_at_index(i as i32)) {
            Some(row) => inner.models_list.select_row(Some(&row)),
            None => inner.models_list.unselect_all(),
        }
        inner.syncing.set(false);
    }

    /// Developer hook: `REALITYSCAN_GTK_SCRIPT="wait:30,action:toggle-log,popup:region,…"`
    /// drives the UI for screenshots and smoke tests; `REALITYSCAN_GTK_SCRIPT=@file` instead
    /// re-runs the file's contents whenever it changes. Steps: `wait:<s>`,
    /// `action:<name>[:<arg>]`, `popup:<region|tools|menu|images>`, `row-menu:<i>` (model row),
    /// `select-image:<i>`, `camera:<index>` (as if that camera was clicked), `close-popups`,
    /// `orbit:<dx>:<dy>`, `zoom:<factor>`, `reset-view`, `simplify:<n>`, `duplicate`,
    /// `select-model:<name>`, `delete-model:<name>`, `export:<path>` (default options),
    /// `region-drag:<+x|-x|+y|-y|-z|+z|centre>:<dx>:<dy>` (pixels), `settings:<key>=<value>`,
    /// `open-image:<i>`, `back-3d`, `new-point:<name>`, `place:<x>:<y>`, `move:<label>:<dx>:<dy>`,
    /// `define-distance:<a>:<b>:<distance>`.
    fn run_script(&self, script: String) {
        let this = self.clone();
        glib::spawn_future_local(async move {
            match script.strip_prefix('@') {
                None => this.run_steps(&script).await,
                Some(file) => {
                    let file = PathBuf::from(file);
                    // Only react to changes made after start-up; stale content is ignored.
                    let mut seen = std::fs::metadata(&file).ok().and_then(|m| m.modified().ok());
                    loop {
                        glib::timeout_future(std::time::Duration::from_millis(500)).await;
                        let Ok(meta) = std::fs::metadata(&file) else { continue };
                        let stamp = meta.modified().ok();
                        if stamp != seen {
                            seen = stamp;
                            if let Ok(text) = std::fs::read_to_string(&file) {
                                this.run_steps(&text).await;
                            }
                        }
                    }
                }
            }
        });
    }

    async fn run_steps(&self, script: &str) {
        let this = self;
        for step in script.split([',', '\n']).map(str::trim).filter(|s| !s.is_empty() && !s.starts_with('#'))
        {
            let mut parts = step.splitn(3, ':');
            let (kind, arg, extra) = (parts.next().unwrap_or(""), parts.next(), parts.next());
            tracing::info!(target: "rs", "script step {step}");
            match (kind, arg) {
                ("wait", Some(secs)) => {
                    let secs: f64 = secs.parse().unwrap_or(1.0);
                    glib::timeout_future(std::time::Duration::from_secs_f64(secs)).await;
                }
                ("action", Some(name)) => {
                    let param = extra.map(|e| e.to_variant());
                    let _ = WidgetExt::activate_action(&this.window, &format!("win.{name}"), param.as_ref());
                }
                ("popup", Some("region")) => this.inner.region_panel.widget().popup(),
                ("popup", Some("align")) => this.inner.align_panel.popover().popup(),
                ("popup", Some(name)) => {
                    if let Some(b) = this.inner.menu_buttons.get(name) {
                        b.popup();
                    }
                }
                ("row-menu", Some(i)) => {
                    let i: usize = i.parse().unwrap_or(0);
                    let menu = this.inner.scene_items.borrow().model_menus.get(i).cloned();
                    if let Some(b) = menu {
                        b.popup();
                    }
                }
                ("select-image", Some(i)) => {
                    let i: i32 = i.parse().unwrap_or(0);
                    if let Some(child) = this.inner.image_grid.child_at_index(i) {
                        this.inner.image_grid.select_child(&child);
                    }
                }
                ("camera", Some(i)) => this.inner.select_image_for_camera(i.parse().ok()),
                ("close-popups", _) => {
                    this.inner.region_panel.widget().popdown();
                    this.inner.align_panel.popover().popdown();
                    for b in this.inner.menu_buttons.values() {
                        b.popdown();
                    }
                    let menus = this.inner.scene_items.borrow().model_menus.clone();
                    for b in menus {
                        b.popdown();
                    }
                }
                ("orbit", Some(dx)) => {
                    let dx: f32 = dx.parse().unwrap_or(0.0);
                    let dy: f32 = extra.and_then(|e| e.parse().ok()).unwrap_or(0.0);
                    this.inner.viewport.nudge(dx, dy, 1.0);
                }
                ("zoom", Some(f)) => this.inner.viewport.nudge(0.0, 0.0, f.parse().unwrap_or(1.0)),
                ("lasso-mode", Some(v)) => this.inner.viewport.set_lasso_mode(v == "on"),
                ("expand-model", Some(i)) => {
                    if let Some(row) = this.inner.models_list.row_at_index(i.parse().unwrap_or(0))
                        && let Ok(row) = row.downcast::<adw::ExpanderRow>()
                    {
                        row.set_expanded(true);
                    }
                }
                ("expand-point", Some(i)) => {
                    if let Some(row) = this.inner.cp_list.row_at_index(i.parse().unwrap_or(0))
                        && let Ok(row) = row.downcast::<adw::ExpanderRow>()
                    {
                        row.set_expanded(true);
                    }
                }
                ("place-on-model", Some(fx)) => {
                    let fy: f64 = extra.and_then(|e| e.parse().ok()).unwrap_or(0.5);
                    this.inner.viewport.script_point_pick(fx.parse().unwrap_or(0.5), fy);
                }
                // lasso:x1/y1;x2/y2;…[:add|sub] — coordinates in logical pixels, or fractions
                // of the viewport when all are within 0..=1.
                ("lasso", Some(points)) => {
                    let mode = match extra {
                        Some("add") => LassoMode::Add,
                        Some("sub") => LassoMode::Subtract,
                        _ => LassoMode::Replace,
                    };
                    let points: Vec<(f64, f64)> = points
                        .split(';')
                        .filter_map(|p| {
                            let (x, y) = p.split_once('/')?;
                            Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
                        })
                        .collect();
                    this.inner.viewport.script_lasso(points, mode);
                }
                ("lasso-edit", Some(which)) => this.inner.viewport.edit_selection(which == "keep"),
                ("lasso-clear", _) => this.inner.viewport.clear_selection(),
                ("reset-view", _) => this.inner.viewport.reset_view(),
                ("log", Some(path)) => {
                    // Dump the log pane (headed by the visible view), so a scripted run can be
                    // checked from a file.
                    let buf = &this.inner.log;
                    let view = this.inner.view_stack.visible_child_name().unwrap_or_default();
                    let text =
                        format!("# view: {view}\n{}", buf.text(&buf.start_iter(), &buf.end_iter(), false));
                    if let Err(e) = std::fs::write(path, text) {
                        eprintln!("script: can't write the log to {path}: {e}");
                    }
                }
                // toggle:cameras[:off] flips a 3D-view display toggle; size:W:H resizes the window;
                // view:3d|image switches the main area.
                ("toggle", Some(what)) => {
                    let on = extra != Some("off");
                    if !this.inner.viewport.set_display(what, on) {
                        eprintln!("script: no display toggle named {what}");
                    }
                }
                ("size", Some(w)) => {
                    let h: i32 = extra.and_then(|e| e.parse().ok()).unwrap_or(800);
                    this.window.set_default_size(w.parse().unwrap_or(1280), h);
                }
                ("view", Some(name)) => this.inner.view_stack.set_visible_child_name(name),
                ("open-image", Some(i)) => this.open_image(i.parse().unwrap_or(0)),
                ("back-3d", _) => this.inner.view_stack.set_visible_child_name("3d"),
                ("new-point", Some(name)) => this.inner.set_active_point(Some(name.to_owned())),
                ("place", Some(x)) => {
                    let y: f64 = extra.and_then(|e| e.parse().ok()).unwrap_or(0.0);
                    this.place_measurement(x.parse().unwrap_or(0.0), y);
                }
                ("mask", Some(step)) => {
                    let args: Vec<f64> =
                        extra.unwrap_or("").split(':').filter_map(|v| v.parse().ok()).collect();
                    this.inner.image_view.script_mask(step, &args);
                }
                ("ortho", Some(side)) => {
                    if let (Some((_, model, textured)), Some(region)) =
                        (this.inner.selected_model(), this.inner.region_panel.region())
                    {
                        let side = match side {
                            "x" => Side::X,
                            "y" => Side::Y,
                            _ => Side::Z,
                        };
                        let px: u32 = extra.and_then(|e| e.parse().ok()).unwrap_or(2048);
                        this.inner.backend.send(Cmd::CalculateOrtho(OrthoRequest {
                            name: format!("Ortho {}", this.inner.orthos.borrow().len() + 1),
                            model,
                            side,
                            rotation: 0,
                            long_side_px: px,
                            textured,
                            region,
                        }));
                    }
                }
                ("view-ortho", Some(i)) => {
                    let name = this
                        .inner
                        .orthos
                        .borrow()
                        .get(i.parse::<usize>().unwrap_or(0))
                        .map(|o| o.name.clone());
                    if let Some(name) = name {
                        this.view_ortho(&name);
                    }
                }
                ("move", Some(label)) => {
                    let mut it = extra.unwrap_or("0:0").split(':');
                    let dx: f64 = it.next().and_then(|v| v.parse().ok()).unwrap_or(0.0);
                    let dy: f64 = it.next().and_then(|v| v.parse().ok()).unwrap_or(0.0);
                    this.inner.image_view.script_move(label, dx, dy);
                }
                ("define-distance", Some(a)) => {
                    let mut it = extra.unwrap_or("").split(':');
                    let b = it.next().unwrap_or("").to_owned();
                    let d: f64 = it.next().and_then(|v| v.parse().ok()).unwrap_or(1.0);
                    this.inner.backend.send(Cmd::DefineDistance {
                        a: a.to_owned(),
                        b,
                        distance: d,
                        name: None,
                    });
                }
                ("settings", Some(pair)) => {
                    let pair = match extra {
                        Some(rest) => format!("{pair}:{rest}"),
                        None => pair.to_owned(),
                    };
                    if let Some((k, v)) = pair.split_once('=') {
                        this.inner.backend.send(Cmd::SetSettings(vec![(k.to_owned(), v.to_owned())]));
                    }
                }
                ("region-drag", Some(handle)) => {
                    let mut it = extra.unwrap_or("0:0").split(':');
                    let dx: f64 = it.next().and_then(|v| v.parse().ok()).unwrap_or(0.0);
                    let dy: f64 = it.next().and_then(|v| v.parse().ok()).unwrap_or(0.0);
                    this.inner.viewport.script_region_drag(handle, dx, dy);
                }
                ("simplify", Some(n)) => {
                    this.inner.backend.send(Cmd::Simplify(n.parse().unwrap_or(1_000_000)))
                }
                ("duplicate", _) => {
                    if let Some((_, name, _)) = this.inner.selected_model() {
                        this.inner.backend.send(Cmd::DuplicateModel(name));
                    }
                }
                ("delete-model", Some(name)) => this.inner.backend.send(Cmd::DeleteModel(name.to_owned())),
                ("select-model", Some(name)) => this.inner.backend.send(Cmd::SelectModel(name.to_owned())),
                ("export", Some(path)) => {
                    if let Some((_, name, _)) = this.inner.selected_model() {
                        let path = PathBuf::from(match extra {
                            Some(rest) => format!("{path}:{rest}"),
                            None => path.to_owned(),
                        });
                        this.inner.backend.send(Cmd::ExportModel {
                            model: name,
                            path,
                            options: rs_scene::export::ModelExportOptions::default(),
                        });
                    }
                }
                _ => tracing::warn!("unknown script step {step}"),
            }
        }
    }

    /// Open image `index` of the scene in the 2D view.
    fn open_image(&self, index: usize) {
        let inner = &self.inner;
        let (local, rs_path, name) = {
            let scene = inner.scene.borrow();
            let Some(img) = scene.as_ref().and_then(|s| s.images.get(index)) else { return };
            let Some(local) = scene.as_ref().and_then(|s| s.image_local_path(img)) else {
                inner.toasts.add_toast(adw::Toast::new("This image's file can't be found on disk"));
                return;
            };
            (local, img.wine_path(), img.file_name())
        };
        let thumb = inner.pictures.borrow().get(&local).and_then(|p| p.file()).and_then(|f| f.path());
        let stem = local.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let mask = ["png", "jpg", "tif", "tiff"]
            .iter()
            .map(|e| crate::backend::cache_dir("masks").join(format!("{stem}.{e}")))
            .find(|p| p.is_file());
        inner.image_view.show(&local, &rs_path, &name, thumb.as_deref(), mask);
        inner.refresh_image_markers();
        inner.view_stack.set_visible_child_name("image");
    }

    /// Ctrl+click on the mesh: measure the active control point in the cameras that see that
    /// spot best (up to three, facing the surface), so RealityScan triangulates it there. A
    /// pinhole projection without lens distortion: refine in the images afterwards.
    fn place_point_on_model(&self, position: glam::Vec3, normal: glam::Vec3) {
        let inner = &self.inner;
        let Some(point) = inner.active_point.borrow().clone() else {
            inner.toasts.add_toast(adw::Toast::new(
                "Choose or create a control point first (Control points › +), then Ctrl+click the model",
            ));
            return;
        };
        let world = [position.x as f64, position.y as f64, position.z as f64];
        let candidates: Vec<PlacementCandidate> = {
            let scene = inner.scene.borrow();
            let cameras = inner.cameras.borrow();
            cameras
                .iter()
                .filter_map(|cam| {
                    let dir = cam.direction_from(world);
                    let to_camera = glam::Vec3::new(dir[0] as f32, dir[1] as f32, dir[2] as f32);
                    let facing = to_camera.dot(normal);
                    if facing <= 0.15 {
                        return None; // looks at the other side of the surface, or along it
                    }
                    let (x, y) = cam.project_inside(world)?;
                    let image = scene.as_ref()?.image_for_camera(cam)?.wine_path();
                    Some(PlacementCandidate { facing, direction: to_camera, image, x, y })
                })
                .collect()
        };
        let rows: Vec<(String, String, f64, f64)> = choose_placement_cameras(&candidates, 3)
            .into_iter()
            .map(|i| (candidates[i].image.clone(), point.clone(), candidates[i].x, candidates[i].y))
            .collect();
        if rows.len() < 2 {
            inner.toasts.add_toast(adw::Toast::new(
                "Fewer than two cameras face that spot; place it in the images instead",
            ));
            return;
        }
        inner.log_line(&format!(
            "{point} picked on the model at {:.4}, {:.4}, {:.4}; measuring it in {}",
            position.x,
            position.y,
            position.z,
            rows.iter().map(|r| r.0.as_str()).collect::<Vec<_>>().join(", ")
        ));
        *inner.pending_placement.borrow_mut() = Some((point.clone(), position));
        inner.backend.send(Cmd::AddMeasurements(rows));
    }

    /// A click in the image view: place a measurement for the active control point.
    fn place_measurement(&self, x: f64, y: f64) {
        let inner = &self.inner;
        let Some(point) = inner.active_point.borrow().clone() else {
            inner
                .toasts
                .add_toast(adw::Toast::new("Choose or create a control point first (Control points › +)"));
            return;
        };
        // The full stored path: a bare file name is ambiguous once two folders share names.
        let Some(image) = inner.image_view.current_image() else { return };
        inner.backend.send(Cmd::AddMeasurement { point, image, x, y });
    }

    /// Right-click on a marker in the image view: delete that measurement after confirming.
    fn remove_measurement(&self, point: &str) {
        let inner = &self.inner;
        let Some(rs_path) = inner.image_view.current_image() else { return };
        let index = inner
            .cps
            .borrow()
            .measurements_on(&rs_path)
            .find(|m| m.point_name == point)
            .map(|m| m.measurement);
        let Some(index) = index else { return };
        let backend = inner.backend.cmds.clone();
        let point = point.to_owned();
        self.confirm(
            &format!("Remove {point} from this image?"),
            "Only this measurement is removed; the control point keeps its other measurements.",
            "_Remove",
            move || {
                let _ = backend.send_blocking(Cmd::DeleteMeasurement { point: point.clone(), index });
            },
        );
    }

    /// A marker was dragged in the image view: re-place that measurement.
    fn move_measurement(&self, point: &str, x: f64, y: f64) {
        let inner = &self.inner;
        let Some(rs_path) = inner.image_view.current_image() else { return };
        let index = inner
            .cps
            .borrow()
            .measurements_on(&rs_path)
            .find(|m| m.point_name == point)
            .map(|m| m.measurement);
        let Some(index) = index else { return };
        let image = rs_path.rsplit(['\\', '/']).next().unwrap_or(&rs_path).to_owned();
        inner.backend.send(Cmd::MoveMeasurement { point: point.to_owned(), index, image, x, y });
    }

    fn filters_for(exts: &[&str]) -> gio::ListStore {
        let filter = gtk::FileFilter::new();
        filter.set_name(Some(&exts.iter().map(|e| format!("*.{e}")).collect::<Vec<_>>().join(", ")));
        for e in exts {
            filter.add_suffix(e);
        }
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);
        filters
    }

    /// Generic save dialog → command.
    fn pick_file_to_export(&self, title: &str, initial: &str, exts: &[&str], make: fn(PathBuf) -> Cmd) {
        let dialog = gtk::FileDialog::builder()
            .title(title)
            .filters(&Self::filters_for(exts))
            .initial_name(initial)
            .modal(true)
            .build();
        let this = self.clone();
        glib::spawn_future_local(async move {
            if let Ok(file) = dialog.save_future(Some(&this.window)).await
                && let Some(path) = file.path()
            {
                this.inner.backend.send(make(path));
            }
        });
    }

    /// Generic open dialog → command.
    fn pick_file_to_import(&self, title: &str, exts: &[&str], make: fn(PathBuf) -> Cmd) {
        let dialog =
            gtk::FileDialog::builder().title(title).filters(&Self::filters_for(exts)).modal(true).build();
        let this = self.clone();
        glib::spawn_future_local(async move {
            if let Ok(file) = dialog.open_future(Some(&this.window)).await
                && let Some(path) = file.path()
            {
                this.inner.backend.send(make(path));
            }
        });
    }

    /// Generic folder dialog → command.
    fn pick_folder_for(&self, title: &str, make: fn(PathBuf) -> Cmd) {
        let dialog = gtk::FileDialog::builder().title(title).modal(true).build();
        let this = self.clone();
        glib::spawn_future_local(async move {
            if let Ok(folder) = dialog.select_folder_future(Some(&this.window)).await
                && let Some(path) = folder.path()
            {
                this.inner.backend.send(make(path));
            }
        });
    }

    /// Dialog for a new orthographic projection of the selected model.
    fn prompt_ortho(&self) {
        let Some((_, model, textured)) = self.inner.selected_model() else { return };
        let Some(region) = self.inner.region_panel.region() else {
            self.inner.toasts.add_toast(adw::Toast::new("No reconstruction region yet"));
            return;
        };
        let sides: Vec<&str> = Side::ALL.iter().map(|s| s.label()).collect();
        let side = gtk::DropDown::from_strings(&sides);
        let rotation = gtk::DropDown::from_strings(&["0°", "90°", "180°", "270°"]);
        let pixels = gtk::SpinButton::with_range(256.0, 32768.0, 256.0);
        pixels.set_value(4096.0);
        let use_texture = gtk::Switch::builder()
            .active(textured)
            .sensitive(textured)
            .valign(gtk::Align::Center)
            .halign(gtk::Align::Start)
            .build();
        let n = self.inner.orthos.borrow().len() + 1;
        let name = gtk::Entry::builder().text(format!("Ortho {n}")).build();
        let grid = gtk::Grid::builder().row_spacing(8).column_spacing(10).build();
        for (row, (label, w)) in [
            ("Name", name.upcast_ref::<gtk::Widget>()),
            ("Looking", side.upcast_ref()),
            ("Rotate image", rotation.upcast_ref()),
            ("Long side (px)", pixels.upcast_ref()),
            ("Use texture", use_texture.upcast_ref()),
        ]
        .into_iter()
        .enumerate()
        {
            let l = gtk::Label::builder().label(label).xalign(0.0).build();
            grid.attach(&l, 0, row as i32, 1, 1);
            grid.attach(w, 1, row as i32, 1, 1);
            w.set_hexpand(true);
        }
        let dialog = adw::AlertDialog::builder()
            .heading(format!("Ortho projection of {model}"))
            .body("Renders the model orthographically through one face of the reconstruction region.")
            .extra_child(&grid)
            .close_response("cancel")
            .default_response("ok")
            .build();
        dialog.add_response("cancel", "_Cancel");
        dialog.add_response("ok", "_Render");
        dialog.set_response_appearance("ok", adw::ResponseAppearance::Suggested);
        let this = self.clone();
        dialog.connect_response(Some("ok"), move |_, _| {
            let req = OrthoRequest {
                name: name.text().trim().to_owned(),
                model: model.clone(),
                side: Side::ALL[side.selected() as usize % Side::ALL.len()],
                rotation: rotation.selected() as u8,
                long_side_px: pixels.value() as u32,
                textured: use_texture.is_active(),
                region: region.clone(),
            };
            this.inner.backend.send(Cmd::CalculateOrtho(req));
        });
        dialog.present(Some(&self.window));
    }

    fn ortho_cache_path(name: &str) -> PathBuf {
        let safe: String = name.chars().map(|c| if c.is_alphanumeric() { c } else { '_' }).collect();
        crate::backend::cache_dir("ortho").join(format!("{safe}.tiff"))
    }

    fn view_ortho(&self, name: &str) {
        let path = Self::ortho_cache_path(name);
        *self.inner.pending_ortho_view.borrow_mut() = Some(path.clone());
        self.inner.backend.send(Cmd::ExportOrtho { name: name.to_owned(), path });
    }

    fn show_orthos(&self, list: Vec<OrthoProjection>) {
        let inner = &self.inner;
        if *inner.orthos.borrow() == list {
            return;
        }
        inner.ortho_list.remove_all();
        for o in &list {
            let row = adw::ActionRow::builder()
                .title(&o.name)
                .subtitle(format!(
                    "{}×{} px · {:.2} × {:.2} units",
                    o.width, o.height, o.box_width, o.box_height
                ))
                .build();
            let view = gtk::Button::from_icon_name("image-x-generic-symbolic");
            view.add_css_class("flat");
            view.set_valign(gtk::Align::Center);
            view.set_tooltip_text(Some("Show in the image view"));
            let this = self.clone();
            let name = o.name.clone();
            view.connect_clicked(move |_| this.view_ortho(&name));
            let export = gtk::Button::from_icon_name("document-save-symbolic");
            export.add_css_class("flat");
            export.set_valign(gtk::Align::Center);
            export.set_tooltip_text(Some("Export as TIFF/PNG/JPEG…"));
            let this = self.clone();
            let name = o.name.clone();
            export.connect_clicked(move |_| {
                let initial =
                    Self::ortho_cache_path(&name).file_name().map(|f| f.to_string_lossy().into_owned());
                let n = name.clone();
                let this2 = this.clone();
                let dialog = gtk::FileDialog::builder()
                    .title("Export ortho projection")
                    .filters(&Self::filters_for(&["tiff", "tif", "png", "jpg"]))
                    .initial_name(initial.unwrap_or_default())
                    .modal(true)
                    .build();
                glib::spawn_future_local(async move {
                    if let Ok(file) = dialog.save_future(Some(&this2.window)).await
                        && let Some(path) = file.path()
                    {
                        this2.inner.backend.send(Cmd::ExportOrtho { name: n.clone(), path });
                    }
                });
            });
            row.add_suffix(&view);
            row.add_suffix(&export);
            inner.ortho_list.append(&row);
        }
        *inner.orthos.borrow_mut() = list;
    }

    fn prompt_new_control_point(&self) {
        let n = self.inner.cps.borrow().points.len() + 1;
        let this = self.clone();
        self.prompt_name_with("New control point", "_Create", &format!("point{n}"), move |name| {
            this.inner.set_active_point(Some(name.clone()));
            this.inner.toasts.add_toast(adw::Toast::new(&format!(
                "Open an image (double-click a thumbnail) and click where {name} is, or Ctrl+click it on the model; two images are enough to triangulate it"
            )));
        });
    }

    /// Dialog for a distance constraint between two control points.
    fn prompt_distance(&self) {
        let names: Vec<String> = self.inner.cps.borrow().points.iter().map(|p| p.name.clone()).collect();
        if names.len() < 2 {
            self.inner
                .toasts
                .add_toast(adw::Toast::new("Two control points are needed to define a distance"));
            return;
        }
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let a = gtk::DropDown::from_strings(&refs);
        let b = gtk::DropDown::from_strings(&refs);
        b.set_selected(1);
        let distance = gtk::SpinButton::with_range(0.0001, 1.0e7, 0.1);
        distance.set_digits(4);
        distance.set_value(1.0);
        let name = gtk::Entry::builder().placeholder_text("Name (optional)").build();
        let grid = gtk::Grid::builder().row_spacing(8).column_spacing(10).build();
        for (row, (label, w)) in [
            ("From", a.upcast_ref::<gtk::Widget>()),
            ("To", b.upcast_ref()),
            ("Distance", distance.upcast_ref()),
            ("Name", name.upcast_ref()),
        ]
        .into_iter()
        .enumerate()
        {
            let l = gtk::Label::builder().label(label).xalign(0.0).build();
            grid.attach(&l, 0, row as i32, 1, 1);
            grid.attach(w, 1, row as i32, 1, 1);
            w.set_hexpand(true);
        }
        let dialog = adw::AlertDialog::builder()
            .heading("Define a distance")
            .body("The known distance between two control points, in the project's units. Apply it with Tools › Update Alignment to Constraints.")
            .extra_child(&grid)
            .close_response("cancel")
            .default_response("ok")
            .build();
        dialog.add_response("cancel", "_Cancel");
        dialog.add_response("ok", "_Define");
        dialog.set_response_appearance("ok", adw::ResponseAppearance::Suggested);
        let this = self.clone();
        dialog.connect_response(Some("ok"), move |_, _| {
            let pick = |d: &gtk::DropDown| names.get(d.selected() as usize).cloned().unwrap_or_default();
            let (pa, pb) = (pick(&a), pick(&b));
            if pa == pb {
                this.inner.toasts.add_toast(adw::Toast::new("Pick two different control points"));
                return;
            }
            let label = name.text().trim().to_owned();
            this.inner.backend.send(Cmd::DefineDistance {
                a: pa,
                b: pb,
                distance: distance.value(),
                name: (!label.is_empty()).then_some(label),
            });
        });
        dialog.present(Some(&self.window));
    }

    fn pick_component_export(&self) {
        let Some((_, name)) = self.inner.selected_component() else { return };
        let filter = gtk::FileFilter::new();
        filter.set_name(Some("RealityScan alignment component (*.rsalign)"));
        filter.add_suffix("rsalign");
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);
        let safe: String = name.chars().map(|c| if c.is_alphanumeric() { c } else { '_' }).collect();
        let dialog = gtk::FileDialog::builder()
            .title("Export component")
            .filters(&filters)
            .initial_name(format!("{safe}.rsalign"))
            .modal(true)
            .build();
        let this = self.clone();
        glib::spawn_future_local(async move {
            if let Ok(file) = dialog.save_future(Some(&this.window)).await
                && let Some(mut path) = file.path()
            {
                if path.extension().is_none_or(|e| e != "rsalign") {
                    path.set_extension("rsalign");
                }
                this.inner.backend.send(Cmd::ExportComponent { component: name, path });
            }
        });
    }

    fn pick_component_import(&self) {
        let filter = gtk::FileFilter::new();
        filter.set_name(Some("RealityScan alignment component (*.rsalign)"));
        filter.add_suffix("rsalign");
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);
        let dialog =
            gtk::FileDialog::builder().title("Import component").filters(&filters).modal(true).build();
        let this = self.clone();
        glib::spawn_future_local(async move {
            if let Ok(file) = dialog.open_future(Some(&this.window)).await
                && let Some(path) = file.path()
            {
                this.inner.backend.send(Cmd::ImportComponent(path));
            }
        });
    }

    /// Rebuild the control-point and distance lists, the viewport markers and the image
    /// view's markers from fresh data.
    fn show_control_points(&self, data: ControlPointData) {
        let inner = &self.inner;
        // Report where a point placed from the 3D view ended up: a large distance means the
        // displayed mesh and RealityScan's cameras are not in the same coordinate frame.
        let placed = inner.pending_placement.borrow().clone();
        if let Some((name, picked)) = placed
            && let Some(p) = data.point_by_name(&name)
            && let Some(pos) = p.position()
        {
            let at = glam::Vec3::new(pos[0] as f32, pos[1] as f32, pos[2] as f32);
            let distance = (at - picked).length();
            inner.log_line(&format!(
                "{name} triangulated at {:.4}, {:.4}, {:.4}: {distance:.4} units from the picked spot{}",
                at.x,
                at.y,
                at.z,
                if distance > 0.05 * picked.length().max(1.0) {
                    " (far: the shown mesh may be from an older alignment; Ctrl+M re-exports it)"
                } else {
                    ""
                }
            ));
            *inner.pending_placement.borrow_mut() = None;
        }
        let changed = *inner.cps.borrow() != data;
        *inner.cps.borrow_mut() = data;
        let cps = inner.cps.borrow();
        if changed {
            inner.syncing.set(true);
            inner.cp_list.remove_all();
            for p in &cps.points {
                let subtitle = match p.position() {
                    Some([x, y, z]) => {
                        let err = p
                            .error
                            .filter(|e| e.is_finite())
                            .map(|e| format!(" · max error {e:.2} px"))
                            .unwrap_or_default();
                        format!(
                            "{} · at {x:.3}, {y:.3}, {z:.3}{err}",
                            count(p.measurement_count as u64, "measurement")
                        )
                    }
                    None => format!(
                        "{} · needs two to triangulate",
                        count(p.measurement_count as u64, "measurement")
                    ),
                };
                let row = adw::ExpanderRow::builder().title(&p.name).subtitle(subtitle).build();
                // Eye: show or hide this point's cross in the 3D view.
                let hidden = inner.hidden_points.borrow().contains(&p.name);
                let eye = gtk::ToggleButton::builder()
                    .icon_name(if hidden { "view-conceal-symbolic" } else { "view-reveal-symbolic" })
                    .active(!hidden)
                    .valign(gtk::Align::Center)
                    .tooltip_text("Show in the 3D view")
                    .build();
                eye.add_css_class("flat");
                let this = self.clone();
                let point_name = p.name.clone();
                eye.connect_toggled(move |b| {
                    b.set_icon_name(if b.is_active() {
                        "view-reveal-symbolic"
                    } else {
                        "view-conceal-symbolic"
                    });
                    {
                        let mut hidden = this.inner.hidden_points.borrow_mut();
                        if b.is_active() {
                            hidden.remove(&point_name);
                        } else {
                            hidden.insert(point_name.clone());
                        }
                    }
                    let active = this.inner.active_point.borrow().clone();
                    this.inner.set_active_point(active);
                });
                row.add_suffix(&eye);
                row.add_suffix(&self.control_point_menu(p.index, &p.name));
                // The images this point is measured in, then (dimmed) the ones it merely
                // projects into; activating either opens the image.
                // Rows name images by their stored path, so same-named files in different
                // folders stay distinct; the path is shown as the tooltip.
                let open_image_row = |this: &MainWindow,
                                      title: &str,
                                      subtitle: &str,
                                      image: String,
                                      dim: bool| {
                    let sub =
                        adw::ActionRow::builder().title(title).subtitle(subtitle).activatable(true).build();
                    sub.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
                    sub.set_tooltip_text(Some(&image));
                    if dim {
                        sub.add_css_class("dim-label");
                    }
                    let this = this.clone();
                    sub.connect_activated(move |_| {
                        let index = this.inner.scene.borrow().as_ref().and_then(|s| {
                            s.images.iter().position(|img| img.wine_path().eq_ignore_ascii_case(&image))
                        });
                        if let Some(i) = index {
                            this.open_image(i);
                        }
                    });
                    sub
                };
                let mut measured: Vec<String> = Vec::new();
                for m in cps.measurements_of(p.index) {
                    let image_name = m.image.rsplit(['\\', '/']).next().unwrap_or(&m.image).to_owned();
                    measured.push(m.image.to_lowercase());
                    row.add_row(&open_image_row(
                        self,
                        &image_name,
                        &format!("{:.1}, {:.1} px", m.x, m.y),
                        m.image.clone(),
                        false,
                    ));
                }
                if let Some(pos) = p.position() {
                    let scene = inner.scene.borrow();
                    for cam in inner.cameras.borrow().iter() {
                        let Some(img) = scene.as_ref().and_then(|s| s.image_for_camera(cam)) else {
                            continue;
                        };
                        let path = img.wine_path();
                        if measured.contains(&path.to_lowercase()) {
                            continue;
                        }
                        let Some((x, y)) = cam.project_inside(pos) else { continue };
                        row.add_row(&open_image_row(
                            self,
                            &img.file_name(),
                            &format!("projected at {x:.0}, {y:.0} px"),
                            path,
                            true,
                        ));
                    }
                }
                row.set_enable_expansion(!measured.is_empty() || p.position().is_some());
                inner.cp_list.append(&row);
            }
            inner.constraint_list.remove_all();
            for (i, c) in cps.constraints.iter().enumerate() {
                let subtitle = if c.calculated.is_finite() && c.calculated > 0.0 {
                    format!(
                        "{} → {}: defined {:.4}, currently {:.4} ({:+.4})",
                        c.a, c.b, c.defined, c.calculated, c.error
                    )
                } else {
                    format!("{} → {}: defined {:.4}", c.a, c.b, c.defined)
                };
                let row = adw::ActionRow::builder().title(&c.name).subtitle(subtitle).build();
                let del = gtk::Button::from_icon_name("user-trash-symbolic");
                del.add_css_class("flat");
                del.set_valign(gtk::Align::Center);
                del.set_tooltip_text(Some("Delete this distance"));
                let backend = inner.backend.cmds.clone();
                del.connect_clicked(move |_| {
                    let _ = backend.send_blocking(Cmd::DeleteConstraint(i as u32));
                });
                row.add_suffix(&del);
                inner.constraint_list.append(&row);
            }
            inner.syncing.set(false);
        }
        // Keep the active point only while it exists (a point with no measurements yet is
        // allowed: it is created by its first click).
        let active = inner.active_point.borrow().clone();
        let active_row = active.as_ref().and_then(|a| cps.points.iter().position(|p| &p.name == a));
        inner.syncing.set(true);
        match active_row.and_then(|i| inner.cp_list.row_at_index(i as i32)) {
            Some(row) => inner.cp_list.select_row(Some(&row)),
            None => inner.cp_list.unselect_all(),
        }
        inner.syncing.set(false);
        let hidden = inner.hidden_points.borrow();
        let markers: Vec<([f32; 3], bool)> = cps
            .points
            .iter()
            .filter(|p| !hidden.contains(&p.name))
            .filter_map(|p| {
                p.position()
                    .map(|q| ([q[0] as f32, q[1] as f32, q[2] as f32], Some(&p.name) == active.as_ref()))
            })
            .collect();
        drop(hidden);
        inner.viewport.set_control_points(&markers);
        drop(cps);
        inner.refresh_image_markers();
    }

    fn control_point_menu(&self, index: u32, name: &str) -> gtk::MenuButton {
        let group = gio::SimpleActionGroup::new();
        let menu = gio::Menu::new();
        let this = self.clone();
        let name = name.to_owned();
        let rename = gio::SimpleAction::new("rename", None);
        rename.connect_activate(glib::clone!(
            #[strong]
            this,
            #[strong]
            name,
            move |_, _| {
                let backend = this.inner.backend.cmds.clone();
                let point = name.clone();
                this.prompt_name(&format!("Rename {name}"), &name, move |new_name| {
                    let _ = backend.send_blocking(Cmd::RenameControlPoint { point: point.clone(), new_name });
                });
            }
        ));
        group.add_action(&rename);
        menu.append(Some("_Rename…"), Some("cp.rename"));
        let delete = gio::SimpleAction::new("delete", None);
        delete.connect_activate(glib::clone!(
            #[strong]
            this,
            move |_, _| {
                let backend = this.inner.backend.cmds.clone();
                this.confirm(
                    &format!("Delete control point “{name}”?"),
                    "Its measurements and any distances using it are removed.",
                    "_Delete",
                    move || {
                        let _ = backend.send_blocking(Cmd::DeleteControlPoint(index));
                    },
                );
            }
        ));
        group.add_action(&delete);
        let danger = gio::Menu::new();
        danger.append(Some("De_lete…"), Some("cp.delete"));
        menu.append_section(None, &danger);
        let button = gtk::MenuButton::builder()
            .icon_name("view-more-symbolic")
            .menu_model(&menu)
            .valign(gtk::Align::Center)
            .build();
        button.add_css_class("flat");
        button.insert_action_group("cp", Some(&group));
        button
    }

    /// Like [`Self::prompt_name`] with a custom heading/verb.
    fn prompt_name_with(&self, heading: &str, verb: &str, initial: &str, on_ok: impl Fn(String) + 'static) {
        let entry = gtk::Entry::builder().text(initial).activates_default(true).build();
        let dialog = adw::AlertDialog::builder()
            .heading(heading)
            .extra_child(&entry)
            .close_response("cancel")
            .default_response("ok")
            .build();
        dialog.add_response("cancel", "_Cancel");
        dialog.add_response("ok", verb);
        dialog.set_response_appearance("ok", adw::ResponseAppearance::Suggested);
        let field = entry.clone();
        dialog.connect_response(Some("ok"), move |_, _| {
            let name = field.text().trim().to_owned();
            if !name.is_empty() {
                on_ok(name);
            }
        });
        dialog.present(Some(&self.window));
        entry.grab_focus();
    }

    /// Per-row menu for a component or model row.
    fn row_menu(&self, kind: RowKind, guid: &str, name: &str, textured: bool) -> gtk::MenuButton {
        let group = gio::SimpleActionGroup::new();
        let menu = gio::Menu::new();
        let this = self.clone();
        let (guid, name) = (guid.to_owned(), name.to_owned());
        let rename = gio::SimpleAction::new("rename", None);
        rename.connect_activate(glib::clone!(
            #[strong]
            this,
            #[strong]
            name,
            move |_, _| {
                let backend = this.inner.backend.cmds.clone();
                let name = name.clone();
                let current = name.clone();
                this.prompt_name(&format!("Rename {}", kind.label()), &current, move |new_name| {
                    let cmd = match kind {
                        RowKind::Component => Cmd::RenameComponent { component: name.clone(), new_name },
                        RowKind::Model => Cmd::RenameModel { model: name.clone(), new_name },
                    };
                    let _ = backend.send_blocking(cmd);
                });
            }
        ));
        group.add_action(&rename);
        menu.append(Some("_Rename…"), Some("row.rename"));
        if kind == RowKind::Model {
            let duplicate = gio::SimpleAction::new("duplicate", None);
            duplicate.connect_activate(glib::clone!(
                #[strong]
                this,
                #[strong]
                name,
                move |_, _| this.inner.backend.send(Cmd::DuplicateModel(name.clone()))
            ));
            group.add_action(&duplicate);
            menu.append(Some("_Duplicate"), Some("row.duplicate"));
            let export = gio::SimpleAction::new("export", None);
            export.connect_activate(glib::clone!(
                #[strong]
                this,
                #[strong]
                name,
                move |_, _| this.export_model(&name, textured)
            ));
            group.add_action(&export);
            menu.append(Some("_Export…"), Some("row.export"));
        }
        let delete = gio::SimpleAction::new("delete", None);
        delete.connect_activate(glib::clone!(
            #[strong]
            this,
            #[strong]
            name,
            move |_, _| {
                let backend = this.inner.backend.cmds.clone();
                let name = name.clone();
                this.confirm(
                    &format!("Delete {} “{name}”?", kind.label()),
                    match kind {
                        RowKind::Component => {
                            "Its cameras, sparse cloud and models are removed from the project."
                        }
                        RowKind::Model => "The model and its textures are removed from the project.",
                    },
                    "_Delete",
                    move || {
                        let cmd = match kind {
                            RowKind::Component => Cmd::DeleteComponent(name.clone()),
                            RowKind::Model => Cmd::DeleteModel(name.clone()),
                        };
                        let _ = backend.send_blocking(cmd);
                    },
                );
            }
        ));
        group.add_action(&delete);
        let danger = gio::Menu::new();
        danger.append(Some("De_lete…"), Some("row.delete"));
        menu.append_section(None, &danger);
        let _ = guid;
        let button = gtk::MenuButton::builder()
            .icon_name("view-more-symbolic")
            .menu_model(&menu)
            .valign(gtk::Align::Center)
            .build();
        button.add_css_class("flat");
        button.insert_action_group("row", Some(&group));
        button
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RowKind {
    Component,
    Model,
}

impl RowKind {
    fn label(self) -> &'static str {
        match self {
            Self::Component => "component",
            Self::Model => "model",
        }
    }
}

fn icon_button(icon: &str, tooltip: &str, action: &str) -> gtk::Button {
    gtk::Button::builder().icon_name(icon).tooltip_text(tooltip).action_name(action).build()
}

/// A section heading with a small "+" button bound to `action`.
fn section_header(text: &str, action: &str, tooltip: &str) -> gtk::Box {
    let row = gtk::Box::builder().orientation(gtk::Orientation::Horizontal).margin_end(6).build();
    let label = section_label(text);
    label.set_hexpand(true);
    let add = gtk::Button::builder()
        .icon_name("list-add-symbolic")
        .action_name(action)
        .tooltip_text(tooltip)
        .build();
    add.add_css_class("flat");
    add.set_valign(gtk::Align::Center);
    row.append(&label);
    row.append(&add);
    row
}

fn section_label(text: &str) -> gtk::Label {
    let l =
        gtk::Label::builder().label(text).xalign(0.0).margin_start(12).margin_top(8).margin_bottom(2).build();
    l.add_css_class("heading");
    l
}

fn count(n: u64, singular: &str) -> String {
    let text = if n >= 1_000_000 {
        format!("{:.1} M", n as f64 / 1e6)
    } else if n >= 10_000 {
        format!("{:.0} k", n as f64 / 1e3)
    } else {
        n.to_string()
    };
    format!("{text} {singular}{}", if n == 1 { "" } else { "s" })
}

impl Inner {
    fn action(&self, name: &str) -> Option<gio::SimpleAction> {
        self.actions.lookup_action(name).and_downcast::<gio::SimpleAction>()
    }

    fn set_connected(&self, connected: bool) {
        self.connected.set(connected);
        for name in ["abort", "refresh"] {
            if let Some(a) = self.action(name) {
                a.set_enabled(connected);
            }
        }
        self.update_actions();
    }

    fn set_busy(&self, busy: bool) {
        if self.busy.replace(busy) != busy {
            self.update_actions();
        }
    }

    /// Enable the command actions according to connection, busy state and what the scene has.
    fn update_actions(&self) {
        let ready = self.connected.get() && !self.busy.get();
        for name in PIPELINE_ACTIONS {
            let enabled = ready
                && (!MODEL_ACTIONS.contains(name) || self.has_model.get())
                && (!COMPONENT_ACTIONS.contains(name) || self.has_component.get());
            if let Some(a) = self.action(name) {
                a.set_enabled(enabled);
            }
        }
        self.region_panel.set_sensitive(ready && self.has_component.get());
    }

    fn show_log(&self) {
        if let Some(a) = self.action("toggle-log")
            && a.state().and_then(|s| s.get::<bool>()) == Some(false)
        {
            a.activate(None);
        }
    }

    fn log_line(&self, text: &str) {
        let mut end = self.log.end_iter();
        self.log.insert(&mut end, text);
        self.log.insert(&mut end, "\n");
        let mark = self.log.create_mark(None, &self.log.end_iter(), false);
        self.log_view.scroll_mark_onscreen(&mark);
        self.log.delete_mark(&mark);
    }

    /// The selected component row, else the scene's selected component: (guid, name).
    fn selected_component(&self) -> Option<(String, String)> {
        let items = self.scene_items.borrow();
        if let Some(row) = self.components_list.selected_row()
            && let Some(c) = items.components.get(row.index() as usize)
        {
            return Some(c.clone());
        }
        items.components.first().cloned()
    }

    /// Make `name` the point that image clicks place; updates the list, markers and hint.
    fn set_active_point(&self, name: Option<String>) {
        *self.active_point.borrow_mut() = name.clone();
        let cps = self.cps.borrow();
        let row = name.as_ref().and_then(|n| cps.points.iter().position(|p| &p.name == n));
        self.syncing.set(true);
        match row.and_then(|i| self.cp_list.row_at_index(i as i32)) {
            Some(r) => self.cp_list.select_row(Some(&r)),
            None => self.cp_list.unselect_all(),
        }
        self.syncing.set(false);
        let hidden = self.hidden_points.borrow();
        let markers: Vec<([f32; 3], bool)> = cps
            .points
            .iter()
            .filter(|p| !hidden.contains(&p.name))
            .filter_map(|p| {
                p.position()
                    .map(|q| ([q[0] as f32, q[1] as f32, q[2] as f32], Some(&p.name) == name.as_ref()))
            })
            .collect();
        drop(hidden);
        self.viewport.set_control_points(&markers);
        drop(cps);
        self.refresh_image_markers();
    }

    /// Markers and hint of the image view for the image it shows.
    fn refresh_image_markers(&self) {
        let Some(rs_path) = self.image_view.current_image() else { return };
        let active = self.active_point.borrow().clone();
        let cps = self.cps.borrow();
        let mut markers: Vec<Marker> = cps
            .measurements_on(&rs_path)
            .map(|m| Marker {
                x: m.x,
                y: m.y,
                label: m.point_name.clone(),
                active: Some(&m.point_name) == active.as_ref(),
                projected: false,
            })
            .collect();
        // Triangulated points that aren't measured here yet, where this camera should see them.
        let camera = {
            let scene = self.scene.borrow();
            let cameras = self.cameras.borrow();
            scene
                .as_ref()
                .and_then(|s| s.image_by_wine_path(&rs_path))
                .and_then(|img| rs_scene::sparse::camera_for_image(&cameras, img))
                .cloned()
        };
        let mut projected = 0;
        if let Some(cam) = camera {
            for p in &cps.points {
                let Some(pos) = p.position() else { continue };
                if markers.iter().any(|m| m.label == p.name) {
                    continue;
                }
                if let Some((x, y)) = cam.project_inside(pos) {
                    markers.push(Marker {
                        x,
                        y,
                        label: p.name.clone(),
                        active: Some(&p.name) == active.as_ref(),
                        projected: true,
                    });
                    projected += 1;
                }
            }
        }
        self.image_view.set_markers(markers);
        let projected_hint = if projected > 0 {
            " · dashed markers: where other images put a point, click one to accept"
        } else {
            ""
        };
        self.image_view.set_hint(&match &active {
            Some(p) => format!(
                "Click to place {p} · right-click a marker to remove it · drag to pan · wheel to zoom{projected_hint}"
            ),
            None => format!("Drag to pan, wheel to zoom. Choose a control point to place measurements.{projected_hint}"),
        });
    }

    /// The selected model row, else the scene's selected model, else the first model.
    fn selected_model(&self) -> Option<(String, String, bool)> {
        let items = self.scene_items.borrow();
        if let Some(row) = self.models_list.selected_row()
            && let Some(m) = items.models.get(row.index() as usize)
        {
            return Some(m.clone());
        }
        items.models.first().cloned()
    }

    /// RealityScan paths of the images selected in the grid.
    fn selected_image_paths(&self) -> Vec<String> {
        let scene = self.scene.borrow();
        let Some(scene) = scene.as_ref() else { return Vec::new() };
        self.image_grid
            .selected_children()
            .iter()
            .filter_map(|c| scene.images.get(c.index() as usize))
            .map(|img| img.wine_path())
            .collect()
    }

    /// Highlight the cameras of the selected images in the viewport.
    fn sync_camera_selection(&self) {
        let scene = self.scene.borrow();
        let cameras = self.cameras.borrow();
        let Some(scene) = scene.as_ref() else { return };
        let indices: Vec<u32> = self
            .image_grid
            .selected_children()
            .iter()
            .filter_map(|c| scene.images.get(c.index() as usize))
            .filter_map(|img| rs_scene::sparse::camera_for_image(&cameras, img).map(|c| c.index))
            .collect();
        self.viewport.set_selected_cameras(&indices);
    }

    /// Cameras caught by a lasso: select their images in the grid (replace, add or remove).
    fn select_images_for_cameras(&self, cameras: &[u32], mode: LassoMode) {
        let targets: Vec<usize> = {
            let scene = self.scene.borrow();
            let cams = self.cameras.borrow();
            cameras
                .iter()
                .filter_map(|idx| cams.iter().find(|c| c.index == *idx))
                .filter_map(|cam| scene.as_ref().and_then(|s| s.image_position_for_camera(cam)))
                .collect()
        };
        if mode == LassoMode::Replace {
            self.image_grid.unselect_all();
        }
        for i in targets {
            if let Some(child) = self.image_grid.child_at_index(i as i32) {
                if mode == LassoMode::Subtract {
                    self.image_grid.unselect_child(&child);
                } else {
                    self.image_grid.select_child(&child);
                }
            }
        }
    }

    /// A camera was clicked in the viewport: select (only) its image in the grid.
    fn select_image_for_camera(&self, camera: Option<u32>) {
        let target = {
            let scene = self.scene.borrow();
            let cameras = self.cameras.borrow();
            camera
                .and_then(|idx| cameras.iter().find(|c| c.index == idx))
                .and_then(|cam| scene.as_ref().and_then(|s| s.image_position_for_camera(cam)))
        };
        self.image_grid.unselect_all();
        if let Some(i) = target
            && let Some(child) = self.image_grid.child_at_index(i as i32)
        {
            self.image_grid.select_child(&child);
            child.grab_focus();
        }
    }

    fn remember_project(&self, scene: &Scene) {
        if scene.project.path.is_empty() {
            return;
        }
        let Some(path) = rs_client::path::from_wine(&scene.project.path, None) else { return };
        let path = path.with_extension("rsproj");
        if !path.is_file() || self.last_recent.borrow().as_ref() == Some(&path) {
            return;
        }
        let list = recent::push(&path);
        *self.last_recent.borrow_mut() = Some(path);
        self.rebuild_recent_menu(list);
    }

    fn rebuild_recent_menu(&self, list: Vec<PathBuf>) {
        self.recent_menu.remove_all();
        if list.is_empty() {
            self.recent_menu.append(Some("No recent projects"), None);
            return;
        }
        for path in list {
            let label = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            let item = gio::MenuItem::new(Some(&label), None);
            item.set_action_and_target_value(
                Some("win.open-recent"),
                Some(&path.to_string_lossy().to_variant()),
            );
            self.recent_menu.append_item(&item);
        }
    }

    fn show_scene(&self, scene: &Scene) {
        let aligned = scene.images.iter().filter(|i| i.aligned).count();
        let has_model = scene.components.iter().any(|c| !c.models.is_empty());
        self.has_model.set(has_model);
        self.has_component.set(scene.project.selected_component().is_some());
        let next_step = if scene.images.is_empty() {
            None
        } else if scene.components.is_empty() {
            Some(0)
        } else if !has_model {
            Some(1)
        } else {
            Some(2)
        };
        for (i, b) in self.step_buttons.iter().enumerate() {
            if next_step == Some(i) {
                b.add_css_class("suggested-action");
            } else {
                b.remove_css_class("suggested-action");
            }
        }
        self.image_count.set_label(&match scene.images.len() {
            0 => "No images".to_owned(),
            n => format!("{n} images · {aligned} aligned"),
        });
        let pending = self.thumbs.pending();
        if pending > 0 {
            self.image_count.set_label(&format!("{} · {pending} thumbnails to go", self.image_count.label()));
        }
        self.image_stack.set_visible_child_name(if scene.images.is_empty() { "empty" } else { "grid" });

        // Rebuild the grid only when the set of images changed; thumbnails are costly. While
        // RealityScan is still adding images, only the new ones are appended.
        let paths: Vec<Option<PathBuf>> =
            scene.images.iter().map(|img| scene.image_local_path(img)).collect();
        let mut pictures = self.pictures.borrow_mut();
        let with_paths = paths.iter().flatten().count();
        let same_set = pictures.len() == with_paths
            && self.cells.borrow().len() == scene.images.len()
            && paths.iter().flatten().all(|p| pictures.contains_key(p));
        let existing = self.cells.borrow().len();
        let grows_only = existing > 0
            && existing < scene.images.len()
            && paths.iter().take(existing).flatten().all(|p| pictures.contains_key(p))
            && pictures.len() == paths.iter().take(existing).flatten().count();
        if with_paths < scene.images.len() {
            tracing::warn!(
                "{} of {} images have no usable local path",
                scene.images.len() - with_paths,
                scene.images.len()
            );
        }
        if same_set {
            // Just refresh the badges.
            for ((badge, flags), img) in self.cells.borrow().iter().zip(&scene.images) {
                badge.set_label(if img.aligned { "aligned" } else { "" });
                flags.set_label(&image_flags_text(img));
            }
            return;
        }
        let start = if grows_only {
            existing
        } else {
            self.image_grid.remove_all();
            pictures.clear();
            self.cells.borrow_mut().clear();
            0
        };
        let mut cells = self.cells.borrow_mut();
        for (img, path) in scene.images.iter().zip(paths).skip(start) {
            let picture = gtk::Picture::builder()
                .content_fit(gtk::ContentFit::Cover)
                .width_request(150)
                .height_request(112)
                .can_shrink(true)
                .build();
            picture.add_css_class("card");
            let name = gtk::Label::builder()
                .label(img.file_name())
                .ellipsize(gtk::pango::EllipsizeMode::Middle)
                .max_width_chars(18)
                .xalign(0.0)
                .build();
            name.add_css_class("caption-heading");
            let meta = gtk::Label::builder()
                .label(format!("{}×{}", img.width, img.height))
                .xalign(0.0)
                .hexpand(true)
                .build();
            meta.add_css_class("caption");
            meta.add_css_class("dim-label");
            let flags = gtk::Label::new(Some(&image_flags_text(img)));
            flags.add_css_class("caption");
            flags.add_css_class("warning");
            let badge = gtk::Label::new(Some(if img.aligned { "aligned" } else { "" }));
            badge.add_css_class("caption");
            badge.add_css_class("accent");
            let meta_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            meta_row.append(&meta);
            meta_row.append(&flags);
            meta_row.append(&badge);
            let cell = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(3).build();
            cell.append(&picture);
            cell.append(&name);
            cell.append(&meta_row);
            cell.set_tooltip_text(Some(&img.wine_path()));
            self.image_grid.append(&cell);
            cells.push((badge, flags));
            if let Some(path) = path {
                pictures.insert(path.clone(), picture);
                self.thumbs.request(path);
            }
        }
    }
}

fn image_flags_text(img: &rs_scene::Image) -> String {
    let mut off = Vec::new();
    if !img.align_enabled {
        off.push("align");
    }
    if !img.mesh_enabled {
        off.push("mesh");
    }
    if !img.texture_enabled {
        off.push("texture");
    }
    if off.is_empty() { String::new() } else { format!("{} off", off.join("/")) }
}

/// A camera that could carry a measurement for a point picked on the model.
struct PlacementCandidate {
    /// Cosine between the surface normal and the direction to the camera.
    facing: f32,
    /// Unit direction from the point to the camera.
    direction: glam::Vec3,
    /// The image's stored path, as the measurement CSV names it.
    image: String,
    x: f64,
    y: f64,
}

/// Up to `n` candidates for triangulating a picked point: the most frontal view first, then
/// whichever remaining view adds the widest angle to those already chosen. Adjacent frames
/// see the point almost along the same ray, so a few pixels of error become a large shift
/// along it; spreading the views keeps RealityScan's triangulation close to the pick.
fn choose_placement_cameras(candidates: &[PlacementCandidate], n: usize) -> Vec<usize> {
    let mut chosen: Vec<usize> = Vec::new();
    let Some(first) =
        (0..candidates.len()).max_by(|&a, &b| candidates[a].facing.total_cmp(&candidates[b].facing))
    else {
        return chosen;
    };
    chosen.push(first);
    while chosen.len() < n {
        let spread = |i: usize| {
            chosen
                .iter()
                .map(|&c| candidates[c].direction.dot(candidates[i].direction).clamp(-1.0, 1.0).acos())
                .fold(f32::INFINITY, f32::min)
        };
        let next = (0..candidates.len())
            .filter(|i| !chosen.contains(i))
            .max_by(|&a, &b| spread(a).total_cmp(&spread(b)));
        match next {
            Some(i) => chosen.push(i),
            None => break,
        }
    }
    chosen
}

#[cfg(test)]
mod placement_tests {
    use super::{PlacementCandidate, choose_placement_cameras};

    #[test]
    fn placement_prefers_the_frontal_view_then_the_widest_baselines() {
        let cand = |facing: f32, x: f32, z: f32| PlacementCandidate {
            facing,
            direction: glam::Vec3::new(x, 0.0, z).normalize(),
            image: String::new(),
            x: 0.0,
            y: 0.0,
        };
        // Three near-identical frontal views and one from 40° off to the side.
        let c =
            vec![cand(0.99, 0.0, 1.0), cand(0.98, 0.05, 1.0), cand(0.97, -0.05, 1.0), cand(0.7, 0.84, 1.0)];
        assert_eq!(choose_placement_cameras(&c, 3), vec![0, 3, 2]);
        assert_eq!(choose_placement_cameras(&c, 9).len(), 4);
        assert!(choose_placement_cameras(&[], 3).is_empty());
    }
}
