//! Popover with the most used alignment settings (RealityScan `-set` keys).

use std::cell::RefCell;
use std::rc::Rc;

use gtk4 as gtk;
use libadwaita as adw;

use adw::prelude::*;

/// One setting: RealityScan key, label, and how it is edited.
pub struct Setting {
    pub key: &'static str,
    pub label: &'static str,
    pub kind: Kind,
    /// RealityScan's default, shown until a value has been applied.
    pub default: &'static str,
}

pub enum Kind {
    Int { min: f64, max: f64, step: f64 },
    Float { min: f64, max: f64, step: f64 },
    Choice(&'static [&'static str]),
    Bool,
}

/// Keys and ranges from RealityScan's "Keys and Values" help page.
pub const ALIGNMENT_SETTINGS: &[Setting] = &[
    Setting {
        key: "sfmFeatureDetectionQuality",
        label: "Feature detection quality",
        kind: Kind::Choice(&["High", "Normal"]),
        default: "High",
    },
    Setting {
        key: "sfmMaxFeaturesPerMpx",
        label: "Max features per megapixel",
        kind: Kind::Int { min: 100.0, max: 100_000.0, step: 500.0 },
        default: "10000",
    },
    Setting {
        key: "sfmMaxFeaturesPerImage",
        label: "Max features per image",
        kind: Kind::Int { min: 1000.0, max: 500_000.0, step: 5000.0 },
        default: "40000",
    },
    Setting {
        key: "sfmImagesOverlap",
        label: "Images' overlap",
        kind: Kind::Choice(&["Low", "Medium", "High"]),
        default: "Medium",
    },
    Setting {
        key: "sfmImageDownscaleFactor",
        label: "Image downscale factor",
        kind: Kind::Int { min: 1.0, max: 8.0, step: 1.0 },
        default: "1",
    },
    Setting {
        key: "sfmMaxFeatureReprojectionError",
        label: "Max reprojection error (px)",
        kind: Kind::Float { min: 0.1, max: 20.0, step: 0.1 },
        default: "2.0",
    },
    Setting {
        key: "sfmPreselectorFeatures",
        label: "Preselector features",
        kind: Kind::Int { min: 100.0, max: 100_000.0, step: 500.0 },
        default: "10000",
    },
    Setting {
        key: "sfmDetectorSensitivity",
        label: "Detector sensitivity",
        kind: Kind::Choice(&["Low", "Medium", "High", "Ultra"]),
        default: "Medium",
    },
    Setting {
        key: "sfmDistortionModel",
        label: "Distortion model",
        kind: Kind::Choice(&[
            "Division",
            "Brown3",
            "Brown4",
            "Brown3WithTangential2",
            "Brown4WithTangential2",
            "KplusBrown3WithTangential2",
            "KplusBrown4WithTangential2",
        ]),
        default: "Brown3",
    },
    Setting {
        key: "sfmAutoReconRegionAfterAlignment",
        label: "Add a reconstruction region after alignment",
        kind: Kind::Bool,
        default: "true",
    },
    Setting {
        key: "sfmForceComponentRematch",
        label: "Force component rematch",
        kind: Kind::Bool,
        default: "false",
    },
];

type ApplyFn = Rc<RefCell<Option<Box<dyn Fn(Vec<(String, String)>)>>>>;

/// Alignment settings saved from earlier sessions.
pub fn load_saved() -> Vec<(String, String)> {
    crate::settings::load()
        .into_iter()
        .filter(|(k, _)| ALIGNMENT_SETTINGS.iter().any(|s| s.key == k))
        .collect()
}

pub fn save(pairs: &[(String, String)]) {
    crate::settings::save(pairs);
}

enum Editor {
    Spin(gtk::SpinButton),
    Combo(adw::ComboRow),
    Switch(adw::SwitchRow),
}

#[derive(Clone)]
pub struct AlignPanel {
    popover: gtk::Popover,
    editors: Rc<Vec<(&'static str, Editor)>>,
    /// Values last applied (or loaded from the store), to send only what changed.
    current: Rc<RefCell<Vec<(String, String)>>>,
    on_apply: ApplyFn,
}

impl AlignPanel {
    pub fn new() -> Self {
        let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).build();
        list.add_css_class("boxed-list");
        let mut editors = Vec::new();
        for s in ALIGNMENT_SETTINGS {
            match &s.kind {
                Kind::Int { min, max, step } | Kind::Float { min, max, step } => {
                    let spin = gtk::SpinButton::with_range(*min, *max, *step);
                    spin.set_digits(if matches!(s.kind, Kind::Float { .. }) { 1 } else { 0 });
                    spin.set_value(s.default.parse().unwrap_or(*min));
                    spin.set_valign(gtk::Align::Center);
                    let row = adw::ActionRow::builder().title(s.label).build();
                    row.add_suffix(&spin);
                    list.append(&row);
                    editors.push((s.key, Editor::Spin(spin)));
                }
                Kind::Choice(options) => {
                    let model = gtk::StringList::new(options);
                    let row = adw::ComboRow::builder().title(s.label).model(&model).build();
                    if let Some(i) = options.iter().position(|o| *o == s.default) {
                        row.set_selected(i as u32);
                    }
                    list.append(&row);
                    editors.push((s.key, Editor::Combo(row)));
                }
                Kind::Bool => {
                    let row = adw::SwitchRow::builder().title(s.label).active(s.default == "true").build();
                    list.append(&row);
                    editors.push((s.key, Editor::Switch(row)));
                }
            }
        }
        let heading = gtk::Label::builder().label("Alignment settings").xalign(0.0).build();
        heading.add_css_class("heading");
        let note = gtk::Label::builder()
            .label("Applied with RealityScan's -set command; they persist in its configuration.")
            .xalign(0.0)
            .wrap(true)
            .max_width_chars(44)
            .build();
        note.add_css_class("dim-label");
        note.add_css_class("caption");
        let apply = gtk::Button::builder().label("Apply").halign(gtk::Align::End).build();
        apply.add_css_class("suggested-action");
        let align =
            gtk::Button::builder().label("Apply and Align").action_name("win.align-with-settings").build();
        let buttons = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .halign(gtk::Align::End)
            .build();
        buttons.append(&apply);
        buttons.append(&align);
        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(10)
            .margin_top(10)
            .margin_bottom(10)
            .margin_start(12)
            .margin_end(12)
            .width_request(420)
            .build();
        content.append(&heading);
        content.append(&list);
        content.append(&note);
        content.append(&buttons);
        let popover = gtk::Popover::builder().child(&content).build();

        let this = Self {
            popover,
            editors: Rc::new(editors),
            current: Rc::new(RefCell::new(Vec::new())),
            on_apply: Rc::new(RefCell::new(None)),
        };
        apply.connect_clicked({
            let this = this.clone();
            move |_| {
                this.apply();
                this.popover.popdown();
            }
        });
        this
    }

    pub fn popover(&self) -> &gtk::Popover {
        &self.popover
    }

    pub fn connect_apply(&self, f: impl Fn(Vec<(String, String)>) + 'static) {
        *self.on_apply.borrow_mut() = Some(Box::new(f));
    }

    /// Send the settings that differ from the last applied values.
    pub fn apply(&self) {
        let current = self.current.borrow();
        let changed: Vec<(String, String)> = self
            .values()
            .into_iter()
            .filter(|(k, v)| current.iter().find(|(ck, _)| ck == k).is_none_or(|(_, cv)| cv != v))
            .collect();
        drop(current);
        if changed.is_empty() {
            return;
        }
        let mut current = self.current.borrow_mut();
        for (k, v) in &changed {
            match current.iter_mut().find(|(ck, _)| ck == k) {
                Some(entry) => entry.1 = v.clone(),
                None => current.push((k.clone(), v.clone())),
            }
        }
        drop(current);
        if let Some(cb) = self.on_apply.borrow().as_ref() {
            cb(changed);
        }
    }

    fn values(&self) -> Vec<(String, String)> {
        self.editors
            .iter()
            .map(|(key, editor)| {
                let value = match editor {
                    Editor::Spin(s) => {
                        if s.digits() == 0 {
                            (s.value() as i64).to_string()
                        } else {
                            format!("{:.1}", s.value())
                        }
                    }
                    Editor::Combo(c) => c
                        .selected_item()
                        .and_downcast::<gtk::StringObject>()
                        .map(|o| o.string().to_string())
                        .unwrap_or_default(),
                    Editor::Switch(s) => if s.is_active() { "true" } else { "false" }.to_owned(),
                };
                ((*key).to_owned(), value)
            })
            .collect()
    }

    /// Show these values (`key`, `value` pairs) as the current ones.
    pub fn set_values(&self, values: &[(String, String)]) {
        *self.current.borrow_mut() = values.to_vec();
        for (key, editor) in self.editors.iter() {
            let Some((_, v)) = values.iter().find(|(k, _)| k == key) else { continue };
            match editor {
                Editor::Spin(s) => {
                    if let Ok(n) = v.parse::<f64>() {
                        s.set_value(n);
                    }
                }
                Editor::Combo(c) => {
                    if let Some(model) = c.model() {
                        let n = model.n_items();
                        for i in 0..n {
                            let item = model.item(i).and_downcast::<gtk::StringObject>();
                            if item.is_some_and(|o| o.string().eq_ignore_ascii_case(v)) {
                                c.set_selected(i);
                                break;
                            }
                        }
                    }
                }
                Editor::Switch(s) => s.set_active(matches!(v.to_ascii_lowercase().as_str(), "true" | "1")),
            }
        }
    }
}
