//! "Export model" flow: options dialog, then a file chooser.

use gtk4 as gtk;
use libadwaita as adw;

use adw::prelude::*;
use gtk::gio;
use rs_scene::export::{MODEL_FORMATS, ModelExportOptions};

/// Ask for export options and a destination, then call `done`.
pub fn run(
    parent: &impl IsA<gtk::Window>,
    model_name: &str,
    textured: bool,
    done: impl Fn(std::path::PathBuf, ModelExportOptions) + 'static,
) {
    let parent = parent.clone().upcast::<gtk::Window>();
    let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).build();
    list.add_css_class("boxed-list");
    // The format is chosen here, not in the file chooser: GTK's chooser doesn't rename the
    // file when its type filter changes, which left exports with the wrong extension.
    let names: Vec<String> = MODEL_FORMATS.iter().map(|(name, ext)| format!("{name} (.{ext})")).collect();
    let name_refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let format = adw::ComboRow::builder()
        .title("Format")
        .model(&gtk::StringList::new(&name_refs))
        .selected(MODEL_FORMATS.iter().position(|(_, ext)| *ext == "obj").unwrap_or(0) as u32)
        .build();
    list.append(&format);
    let make = |title: &str, subtitle: &str, active: bool| {
        let row = adw::SwitchRow::builder().title(title).subtitle(subtitle).active(active).build();
        list.append(&row);
        row
    };
    let colors = make("Vertex colours", "Per-vertex colour, if the model has been coloured", true);
    let normals = make("Vertex normals", "", true);
    let textures = make("Textures", "Write the texture image(s) next to the model", textured);
    textures.set_sensitive(textured);
    let embed = make("Embed textures", "Put textures inside the model file (glb, fbx, usdz)", false);
    embed.set_sensitive(textured && embeds_textures(selected_ext(&format)));
    {
        let embed = embed.clone();
        format.connect_selected_notify(move |row| {
            embed.set_sensitive(textured && embeds_textures(selected_ext(row)));
        });
    }
    let one = make("Single texture file", "Stitch several textures into one image", false);
    one.set_sensitive(textured);
    let info = make("Write .rsInfo", "Coordinate-system file for re-importing the result", false);

    let dialog = adw::AlertDialog::builder()
        .heading(format!("Export {model_name}"))
        .extra_child(&list)
        .close_response("cancel")
        .default_response("next")
        .build();
    dialog.add_response("cancel", "_Cancel");
    dialog.add_response("next", "_Choose File…");
    dialog.set_response_appearance("next", adw::ResponseAppearance::Suggested);
    let model_name = model_name.to_owned();
    let done = std::rc::Rc::new(done);
    let owner = parent.clone();
    dialog.connect_response(None, move |_, response| {
        if response != "next" {
            return;
        }
        let options = ModelExportOptions {
            vertex_colors: colors.is_active(),
            vertex_normals: normals.is_active(),
            textures: textures.is_active(),
            embed_textures: embed.is_active(),
            one_texture: one.is_active(),
            info_file: info.is_active(),
            binary: true,
        };
        let ext = selected_ext(&format);
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        let mut chosen = None;
        for (name, e) in MODEL_FORMATS {
            let f = gtk::FileFilter::new();
            f.set_name(Some(&format!("{name} (*.{e})")));
            f.add_suffix(e);
            if *e == ext {
                chosen = Some(f.clone());
            }
            filters.append(&f);
        }
        let safe: String = model_name.chars().map(|c| if c.is_alphanumeric() { c } else { '_' }).collect();
        let mut builder = gtk::FileDialog::builder()
            .title("Export model")
            .filters(&filters)
            .initial_name(format!("{safe}.{ext}"))
            .modal(true);
        if let Some(f) = chosen {
            builder = builder.default_filter(&f);
        }
        let chooser = builder.build();
        let done = done.clone();
        chooser.save(Some(&owner), gio::Cancellable::NONE, move |result| {
            if let Ok(file) = result
                && let Some(path) = file.path()
            {
                done(with_format(path, ext), options.clone());
            }
        });
    });
    dialog.present(Some(&parent));
}

/// Extension of the format selected in the combo row.
fn selected_ext(row: &adw::ComboRow) -> &'static str {
    MODEL_FORMATS.get(row.selected() as usize).map_or("obj", |(_, ext)| ext)
}

/// Formats that can carry their textures inside the file.
fn embeds_textures(ext: &str) -> bool {
    matches!(ext, "glb" | "fbx" | "usdz")
}

/// The chosen path with the selected format's extension, unless the user typed another model
/// format's extension themselves (that one wins), so "model" or "model.txt" become "model.obj"
/// while "model.ply" stays as typed.
fn with_format(mut path: std::path::PathBuf, ext: &str) -> std::path::PathBuf {
    let typed = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase());
    let known = typed.as_deref().is_some_and(|t| MODEL_FORMATS.iter().any(|(_, e)| *e == t));
    if !known {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        path.set_file_name(format!("{name}.{ext}"));
    }
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_chosen_format_sets_the_extension_unless_a_model_extension_was_typed() {
        let p = |s: &str| std::path::PathBuf::from(s);
        assert_eq!(with_format(p("/x/model"), "glb"), p("/x/model.glb"));
        assert_eq!(with_format(p("/x/model.txt"), "glb"), p("/x/model.txt.glb"));
        assert_eq!(with_format(p("/x/model.PLY"), "glb"), p("/x/model.PLY"));
        assert_eq!(with_format(p("/x/v1.2"), "obj"), p("/x/v1.2.obj"));
        assert!(embeds_textures("usdz") && !embeds_textures("obj"));
    }
}
