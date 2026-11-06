//! Popover for the reconstruction region: automatic choices plus numeric editing.

use std::cell::RefCell;
use std::rc::Rc;

use gtk4 as gtk;

use gtk::prelude::*;
use rs_scene::region::Region;

type ApplyFn = Rc<RefCell<Option<Box<dyn Fn(Region)>>>>;

#[derive(Clone)]
pub struct RegionPanel {
    button: gtk::MenuButton,
    centre: [gtk::SpinButton; 3],
    size: [gtk::SpinButton; 3],
    ypr: [gtk::SpinButton; 3],
    apply: gtk::Button,
    info: gtk::Label,
    region: Rc<RefCell<Option<Region>>>,
    on_apply: ApplyFn,
}

impl RegionPanel {
    pub fn new() -> Self {
        let grid = gtk::Grid::builder().row_spacing(6).column_spacing(8).margin_top(6).build();
        let spin = |step: f64, lo: f64, hi: f64| {
            gtk::SpinButton::builder()
                .adjustment(&gtk::Adjustment::new(0.0, lo, hi, step, step * 10.0, 0.0))
                .digits(3)
                .width_chars(9)
                .build()
        };
        let centre = [spin(0.1, -1e6, 1e6), spin(0.1, -1e6, 1e6), spin(0.1, -1e6, 1e6)];
        let size = [spin(0.1, 0.001, 1e6), spin(0.1, 0.001, 1e6), spin(0.1, 0.001, 1e6)];
        let ypr = [spin(1.0, -360.0, 360.0), spin(1.0, -360.0, 360.0), spin(1.0, -360.0, 360.0)];
        for (col, h) in ["", "X", "Y", "Z"].iter().enumerate() {
            let l = gtk::Label::new(Some(h));
            l.add_css_class("dim-label");
            grid.attach(&l, col as i32, 0, 1, 1);
        }
        for (row, (label, spins)) in
            [("Centre", &centre), ("Size", &size), ("Yaw / pitch / roll", &ypr)].into_iter().enumerate()
        {
            let l = gtk::Label::builder().label(label).xalign(0.0).build();
            grid.attach(&l, 0, row as i32 + 1, 1, 1);
            for (col, s) in spins.iter().enumerate() {
                grid.attach(s, col as i32 + 1, row as i32 + 1, 1, 1);
            }
        }

        let auto = gtk::Button::builder().label("Auto").action_name("win.region-auto").hexpand(true).build();
        auto.set_tooltip_text(Some("Fit the region to the whole sparse cloud"));
        let density = gtk::Button::builder()
            .label("By density")
            .action_name("win.region-density")
            .hexpand(true)
            .build();
        density.set_tooltip_text(Some("Fit the region to the densest part of the sparse cloud"));
        let presets = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        presets.add_css_class("linked");
        presets.append(&auto);
        presets.append(&density);

        let apply = gtk::Button::builder().label("Apply").halign(gtk::Align::End).sensitive(false).build();
        apply.add_css_class("suggested-action");
        let info = gtk::Label::builder().xalign(0.0).wrap(true).max_width_chars(36).build();
        info.add_css_class("dim-label");
        info.add_css_class("caption");

        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(10)
            .margin_top(10)
            .margin_bottom(10)
            .margin_start(12)
            .margin_end(12)
            .build();
        let heading = gtk::Label::builder().label("Reconstruction region").xalign(0.0).build();
        heading.add_css_class("heading");
        content.append(&heading);
        content.append(&presets);
        content.append(&grid);
        content.append(&info);
        content.append(&apply);

        let popover = gtk::Popover::builder().child(&content).build();
        let button = gtk::MenuButton::builder()
            .label("Region")
            .popover(&popover)
            .tooltip_text("Reconstruction region: automatic fit or exact values")
            .build();

        let this = Self {
            button,
            centre,
            size,
            ypr,
            apply,
            info,
            region: Rc::new(RefCell::new(None)),
            on_apply: Rc::new(RefCell::new(None)),
        };
        this.apply.connect_clicked({
            let this = this.clone();
            move |_| {
                let Some(mut region) = this.region.borrow().clone() else { return };
                region.centre = this.centre.each_ref().map(|s| s.value());
                region.size = this.size.each_ref().map(|s| s.value());
                region.ypr = this.ypr.each_ref().map(|s| s.value());
                if let Some(cb) = this.on_apply.borrow().as_ref() {
                    cb(region);
                }
                this.button.popdown();
            }
        });
        this
    }

    pub fn widget(&self) -> &gtk::MenuButton {
        &self.button
    }

    pub fn connect_apply(&self, f: impl Fn(Region) + 'static) {
        *self.on_apply.borrow_mut() = Some(Box::new(f));
    }

    /// Show a region's values (or an empty panel for `None`).
    pub fn set_region(&self, region: Option<&Region>) {
        *self.region.borrow_mut() = region.cloned();
        self.apply.set_sensitive(region.is_some());
        match region {
            Some(r) => {
                for (s, v) in self.centre.iter().zip(r.centre) {
                    s.set_value(v);
                }
                for (s, v) in self.size.iter().zip(r.size) {
                    s.set_value(v);
                }
                for (s, v) in self.ypr.iter().zip(r.ypr) {
                    s.set_value(v);
                }
                self.info.set_label(&format!(
                    "Volume {:.3} units³. Models are reconstructed only inside this box.",
                    r.size[0] * r.size[1] * r.size[2]
                ));
            }
            None => self.info.set_label("No component selected, so there is no region yet."),
        }
        for s in self.centre.iter().chain(&self.size).chain(&self.ypr) {
            s.set_sensitive(region.is_some());
        }
    }

    /// The region currently shown.
    pub fn region(&self) -> Option<Region> {
        self.region.borrow().clone()
    }

    pub fn set_sensitive(&self, sensitive: bool) {
        self.button.set_sensitive(sensitive);
    }
}
