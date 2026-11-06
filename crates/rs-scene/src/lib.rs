//! Scene model mirrored from a running RealityScan instance.
//!
//! RealityScan's CLI is write-only, so state is read back by evaluating a
//! report template (`templates/state.tpl`) through the Remote Command plugin's
//! `/template` endpoint. The template emits JSON; this crate parses it into
//! plain Rust types.

use serde::Deserialize;

pub mod controlpoints;
pub mod export;
pub mod gltf;
pub mod mesh;
pub mod ortho;
pub mod region;
pub mod sparse;

/// Report template that renders the project state as JSON.
pub const STATE_TEMPLATE: &str = include_str!("../templates/state.tpl");
/// Report template with colour/texture details of the selected component's models. Needs
/// the `ModelExportFunctionSet`, which errors when no model is selected, so it is only
/// evaluated when the state reports one.
pub const MODELS_TEMPLATE: &str = include_str!("../templates/models.tpl");

/// RealityScan's "no object" GUID, used for the selected component/model when none exists.
pub const NULL_GUID: &str = "{00000000-0000-0000-0000-000000000000}";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("state template output is not the expected JSON: {0}")]
    Parse(#[from] serde_json::Error),
    #[error(transparent)]
    Client(#[from] rs_client::Error),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scene {
    /// e.g. `2.2.0.119430 RS`
    pub version: String,
    /// Registered cameras in the selected component (RealityScan reports 1 when there is none).
    pub selected_camera_count: u32,
    /// Sparse points in the selected component (likewise 1 when there is none).
    pub selected_point_count: u64,
    pub project: Project,
    pub images: Vec<Image>,
    pub components: Vec<Component>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub name: String,
    /// Windows path of the `.rsproj`, empty for an unsaved project.
    pub path: String,
    pub guid: String,
    /// Increments with every change; compare with `Status::change_counter`.
    pub change_count: u64,
    pub image_count: u32,
    pub component_count: u32,
    selected_component: String,
    selected_model: String,
}

impl Project {
    pub fn selected_component(&self) -> Option<&str> {
        non_null_guid(&self.selected_component)
    }

    pub fn selected_model(&self) -> Option<&str> {
        non_null_guid(&self.selected_model)
    }
}

/// An input image as RealityScan lists it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Image {
    pub index: u32,
    /// File name without extension.
    pub name: String,
    /// Extension including the dot, e.g. `.jxl`.
    pub ext: String,
    /// Windows directory path, with a trailing backslash.
    pub dir: String,
    pub width: u32,
    pub height: u32,
    /// Aligned in the *selected* component.
    pub aligned: bool,
    /// Camera index in the selected component, when aligned.
    #[serde(deserialize_with = "neg_is_none")]
    pub camera_index: Option<u32>,
    pub align_enabled: bool,
    pub mesh_enabled: bool,
    pub texture_enabled: bool,
}

impl Image {
    pub fn file_name(&self) -> String {
        format!("{}{}", self.name, self.ext)
    }

    /// Full Windows path as RealityScan sees it.
    pub fn wine_path(&self) -> String {
        format!("{}{}{}", self.dir, self.name, self.ext)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Component {
    pub guid: String,
    pub name: String,
    pub camera_count: u32,
    pub models: Vec<Model>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Model {
    pub guid: String,
    pub name: String,
    pub triangles: u64,
    pub vertices: u64,
    /// Colour/texture details, known for the models of the selected component.
    #[serde(skip)]
    pub info: Option<ModelInfo>,
}

impl Model {
    pub fn is_textured(&self) -> bool {
        self.info.as_ref().is_some_and(|i| i.textured)
    }

    pub fn is_colored(&self) -> bool {
        self.info.as_ref().is_some_and(|i| i.colored)
    }
}

/// Output of [`MODELS_TEMPLATE`] for one model.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    pub guid: String,
    pub parts: u32,
    pub colored: bool,
    pub textured: bool,
    #[serde(default)]
    pub texture_count: u32,
    #[serde(default)]
    pub texture_width: u32,
    #[serde(default)]
    pub texture_height: u32,
}

impl Scene {
    /// The image a registered camera was computed from: by RealityScan's input index, which
    /// keeps same-named files in different folders apart, or by name when that name is unique.
    pub fn image_for_camera(&self, cam: &sparse::CameraPose) -> Option<&Image> {
        self.image_position_for_camera(cam).map(|i| &self.images[i])
    }

    /// Position in `images` of the image a camera was computed from (see [`Self::image_for_camera`]).
    pub fn image_position_for_camera(&self, cam: &sparse::CameraPose) -> Option<usize> {
        if let Some(i) = self.images.iter().position(|img| img.index == cam.image_index) {
            return Some(i);
        }
        let mut named =
            self.images.iter().enumerate().filter(|(_, img)| img.name.eq_ignore_ascii_case(&cam.image));
        match (named.next(), named.next()) {
            (Some((i, _)), None) => Some(i),
            _ => None,
        }
    }

    /// The image stored under a RealityScan path (`dir + name + ext`), compared case-insensitively.
    pub fn image_by_wine_path(&self, path: &str) -> Option<&Image> {
        self.images.iter().find(|img| img.wine_path().eq_ignore_ascii_case(path))
    }

    /// Local (Linux) path of an image.
    ///
    /// Saved projects store image folders relative to the project, but the number of `..`
    /// levels RealityScan writes is not reliable (3 in one save, 9 in the next for the same
    /// folder). So the `..` prefix is dropped and the remaining tail is looked for in the
    /// project folder and each of its ancestors; the first existing file wins. Falls back to
    /// the literal resolution when nothing exists yet.
    pub fn image_local_path(&self, img: &Image) -> Option<std::path::PathBuf> {
        let win = img.wine_path();
        if win.contains(':') {
            return rs_client::path::from_wine(&win, None);
        }
        // `project.path` is `<dir>\<name>` without the .rsproj extension.
        let project = rs_client::path::from_wine(&self.project.path, None)?;
        let dir = project.parent()?;
        let rel = win.replace('\\', "/");
        let tail: std::path::PathBuf = std::path::Path::new(&rel)
            .components()
            .skip_while(|c| matches!(c, std::path::Component::ParentDir | std::path::Component::CurDir))
            .collect();
        let literal = normalize(dir.join(&rel));
        if literal.is_file() {
            return Some(literal);
        }
        for base in dir.ancestors() {
            let candidate = base.join(&tail);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        Some(literal)
    }
}

/// Resolve `.` and `..` components lexically.
fn normalize(path: std::path::PathBuf) -> std::path::PathBuf {
    use std::path::Component;
    let mut out = std::path::PathBuf::new();
    for c in path.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

fn non_null_guid(guid: &str) -> Option<&str> {
    (!guid.is_empty() && guid != NULL_GUID).then_some(guid)
}

fn neg_is_none<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<u32>, D::Error> {
    let v = i64::deserialize(d)?;
    Ok(u32::try_from(v).ok())
}

// The template ends every list with a `null` so that empty lists and trailing
// commas stay valid JSON; these raw types absorb that.

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawScene {
    version: String,
    #[serde(default)]
    selected_camera_count: u32,
    #[serde(default)]
    selected_point_count: u64,
    project: Project,
    images: Vec<Option<Image>>,
    components: Vec<Option<RawComponent>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawComponent {
    guid: String,
    name: String,
    camera_count: u32,
    models: Vec<Option<Model>>,
}

/// Parse the output of [`STATE_TEMPLATE`].
pub fn parse_state(text: &str) -> Result<Scene, Error> {
    let raw: RawScene = serde_json::from_str(&rs_client::json::sanitize(text))?;
    Ok(Scene {
        version: raw.version,
        selected_camera_count: raw.selected_camera_count,
        selected_point_count: raw.selected_point_count,
        project: raw.project,
        images: raw.images.into_iter().flatten().collect(),
        components: raw
            .components
            .into_iter()
            .flatten()
            .map(|c| Component {
                guid: c.guid,
                name: c.name,
                camera_count: c.camera_count,
                models: c.models.into_iter().flatten().collect(),
            })
            .collect(),
    })
}

/// Parse the output of [`MODELS_TEMPLATE`].
pub fn parse_model_info(text: &str) -> Result<Vec<ModelInfo>, Error> {
    let raw: Vec<Option<ModelInfo>> = serde_json::from_str(&rs_client::json::sanitize(text))?;
    Ok(raw.into_iter().flatten().collect())
}

impl Scene {
    /// Attach [`ModelInfo`] records to the matching models.
    pub fn merge_model_info(&mut self, infos: Vec<ModelInfo>) {
        for info in infos {
            for m in self.components.iter_mut().flat_map(|c| c.models.iter_mut()) {
                if m.guid == info.guid {
                    m.info = Some(info.clone());
                }
            }
        }
    }
}

/// Query a running instance for its current state (without model details).
pub async fn fetch_scene(client: &rs_client::Client) -> Result<Scene, Error> {
    let text = client.template(STATE_TEMPLATE).await?;
    parse_state(&text)
}

/// Query colour/texture details of the selected component's models. Only call this when the
/// selected component has at least one model: the function set errors otherwise, and a
/// failing template evaluation counts as a failed process (status error, sequence event) in
/// RealityScan.
pub async fn fetch_model_info(client: &rs_client::Client) -> Result<Vec<ModelInfo>, Error> {
    let text = client.template(MODELS_TEMPLATE).await?;
    parse_model_info(&text)
}

/// Query a running instance for its current state, including model details when a model
/// is selected.
pub async fn fetch_state(client: &rs_client::Client) -> Result<Scene, Error> {
    let mut scene = fetch_scene(client).await?;
    let has_models = scene
        .project
        .selected_component()
        .and_then(|guid| scene.components.iter().find(|c| c.guid == guid))
        .is_some_and(|c| !c.models.is_empty());
    if has_models {
        match fetch_model_info(client).await {
            Ok(infos) => scene.merge_model_info(infos),
            Err(e) => tracing::warn!("model info query failed: {e}"),
        }
    }
    Ok(scene)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ONE_IMAGE: &str = r#"{"version":"2.2.0.119430 RS",
"project":{"name":"New project","path":"","guid":"{F9D890F7-0EAF-43DF-A6A0-B5C8546B2490}","changeCount":1,"imageCount":1,"componentCount":0,"selectedComponent":"{00000000-0000-0000-0000-000000000000}","selectedModel":"{00000000-0000-0000-0000-000000000000}"},
"images":[{"index":0,"name":"jxl_test","ext":".jxl","dir":"Z:\\home\\pfalkingham\\Games\\Heroic\\Prefixes\\RealityScan 2.2\\","width":800,"height":600,"aligned":false,"cameraIndex":-1,"alignEnabled":true,"meshEnabled":true,"textureEnabled":true},null],
"components":[null]}"#;

    #[test]
    fn parses_project_with_one_image_and_no_components() {
        let scene = parse_state(ONE_IMAGE).unwrap();
        assert_eq!(scene.version, "2.2.0.119430 RS");
        assert_eq!(scene.project.name, "New project");
        assert_eq!(scene.project.selected_component(), None);
        assert_eq!(scene.project.selected_model(), None);
        assert_eq!(scene.images.len(), 1);
        let img = &scene.images[0];
        assert_eq!(img.file_name(), "jxl_test.jxl");
        assert_eq!(
            img.wine_path(),
            r"Z:\home\pfalkingham\Games\Heroic\Prefixes\RealityScan 2.2\jxl_test.jxl"
        );
        assert_eq!(img.camera_index, None);
        assert!(!img.aligned && img.align_enabled);
        assert!(scene.components.is_empty());
    }

    #[test]
    fn cameras_map_to_images_by_input_index_not_name() {
        // Two folders holding files with the same names, as RealityScan allows.
        let text = r#"{"version":"x","project":{"name":"p","path":"","guid":"{1}","changeCount":1,"imageCount":3,"componentCount":0,"selectedComponent":"{0}","selectedModel":"{0}"},
"images":[{"index":0,"name":"image_006","ext":".jxl","dir":"Z:\\a\\","width":8,"height":6,"aligned":true,"cameraIndex":0,"alignEnabled":true,"meshEnabled":true,"textureEnabled":true},
{"index":1,"name":"image_006","ext":".jxl","dir":"Z:\\b\\","width":8,"height":6,"aligned":true,"cameraIndex":1,"alignEnabled":true,"meshEnabled":true,"textureEnabled":true},
{"index":2,"name":"image_007","ext":".jxl","dir":"Z:\\b\\","width":8,"height":6,"aligned":false,"cameraIndex":-1,"alignEnabled":true,"meshEnabled":true,"textureEnabled":true},null],
"components":[null]}"#;
        let scene = parse_state(text).unwrap();
        let cam = |index, image_index, image: &str| sparse::CameraPose {
            index,
            image_index,
            image: image.into(),
            width: 8,
            height: 6,
            position: [0.0; 3],
            rotation: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
            focal: 1.0,
            principal: [0.0; 2],
            radial: [0.0; 4],
            tangential: [0.0; 2],
        };
        let cams = vec![cam(0, 0, "image_006"), cam(1, 1, "image_006")];
        assert_eq!(
            scene.image_for_camera(&cams[1]).map(|i| i.wine_path()),
            Some(r"Z:\b\image_006.jxl".into())
        );
        assert_eq!(scene.image_position_for_camera(&cams[0]), Some(0));
        // An unknown index falls back to the name only when it is unique.
        assert_eq!(scene.image_position_for_camera(&cam(5, 99, "image_007")), Some(2));
        assert_eq!(scene.image_position_for_camera(&cam(5, 99, "image_006")), None);
        assert_eq!(sparse::camera_for_image(&cams, &scene.images[1]).map(|c| c.index), Some(1));
        assert_eq!(sparse::camera_for_image(&cams, &scene.images[2]), None);
        assert_eq!(scene.image_by_wine_path(r"z:\B\IMAGE_006.JXL").map(|i| i.index), Some(1));
    }

    #[test]
    fn parses_components_and_models() {
        let text = r#"{"version":"x","project":{"name":"p","path":"Z:\\p.rsproj","guid":"{1}","changeCount":9,"imageCount":2,"componentCount":1,"selectedComponent":"{C1}","selectedModel":"{M1}"},
"images":[null],
"components":[{"guid":"{C1}","name":"Component 0","cameraCount":2,"models":[{"guid":"{M1}","name":"Model 1","triangles":1200000,"vertices":600123},null]},null]}"#;
        let scene = parse_state(text).unwrap();
        assert_eq!(scene.project.selected_component(), Some("{C1}"));
        assert_eq!(scene.project.selected_model(), Some("{M1}"));
        assert_eq!(scene.components.len(), 1);
        assert_eq!(scene.components[0].models[0].triangles, 1_200_000);
    }

    #[test]
    fn resolves_project_relative_image_paths_by_searching_ancestors() {
        // Lay out: <tmp>/home/me/Photog/set a/image_001.jxl and a project in
        // <tmp>/home/me/.cache/rs/projects/p; RealityScan's `..` count is deliberately wrong.
        let tmp = std::env::temp_dir().join(format!("rs-scene-test-{}", std::process::id()));
        let photos = tmp.join("home/me/Photog/set a");
        let projects = tmp.join("home/me/.cache/rs/projects");
        std::fs::create_dir_all(&photos).unwrap();
        std::fs::create_dir_all(&projects).unwrap();
        std::fs::write(photos.join("image_001.jxl"), b"x").unwrap();
        let project_win = rs_client::path::to_wine(&projects.join("p")).unwrap().replace('\\', "\\\\");
        let text = format!(
            r#"{{"version":"x","project":{{"name":"p","path":"{project_win}","guid":"{{1}}","changeCount":1,"imageCount":1,"componentCount":0,"selectedComponent":"","selectedModel":""}},
"images":[{{"index":0,"name":"image_001","ext":".jxl","dir":"..\\..\\..\\..\\..\\..\\..\\..\\..\\Photog\\set a\\","width":1,"height":1,"aligned":false,"cameraIndex":-1,"alignEnabled":true,"meshEnabled":true,"textureEnabled":true}},null],
"components":[null]}}"#
        );
        let scene = parse_state(&text).unwrap();
        assert_eq!(scene.image_local_path(&scene.images[0]), Some(photos.join("image_001.jxl")));
        let abs = Image { dir: r"Z:\data\".into(), ..scene.images[0].clone() };
        assert_eq!(scene.image_local_path(&abs), Some(std::path::PathBuf::from("/data/image_001.jxl")));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn parses_and_merges_model_info() {
        let text = r#"{"version":"x","project":{"name":"p","path":"Z:\\p.rsproj","guid":"{1}","changeCount":9,"imageCount":2,"componentCount":1,"selectedComponent":"{C1}","selectedModel":"{M1}"},
"images":[null],
"components":[{"guid":"{C1}","name":"Component 0","cameraCount":2,"models":[{"guid":"{M1}","name":"Model 1","triangles":1200000,"vertices":600123},null]},null]}"#;
        let mut scene = parse_state(text).unwrap();
        assert!(!scene.components[0].models[0].is_textured());
        let infos = parse_model_info(
            r#"[{"guid":"{M1}","parts":1,"colored":true,"textured":true,"textureCount":1,"textureWidth":8192,"textureHeight":8192},null]"#,
        )
        .unwrap();
        scene.merge_model_info(infos);
        let m = &scene.components[0].models[0];
        assert!(m.is_textured() && m.is_colored());
        assert_eq!(m.info.as_ref().unwrap().texture_width, 8192);
    }

    #[test]
    fn rejects_garbage() {
        assert!(matches!(parse_state("not json"), Err(Error::Parse(_))));
    }

    #[test]
    fn template_lists_end_with_null_sentinels() {
        // Guards against someone "fixing" the template into Epic's $Strip(1) style,
        // which breaks on empty lists.
        assert!(STATE_TEMPLATE.contains(",)null]"));
        assert!(!STATE_TEMPLATE.contains("$Strip("));
    }
}
