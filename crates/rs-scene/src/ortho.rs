//! Orthographic projections: the `.rsortho` parameter file for `calculateOrthoProjection`
//! and the template that lists existing projections.
//!
//! A projection looks through one face of a reconstruction region; `boxSideConerIndex`
//! (0–23, RealityScan's spelling) picks the face and the corner that becomes the top-left
//! pixel. Observed on an axis-aligned box: index 0 looks along X (image = Y × Z), 8 along Y
//! (Z × X), 16 along Z (X × Y); the index within each group of four rotates the image.

use serde::Deserialize;

use crate::Error;
use crate::region::Region;

pub const ORTHO_TEMPLATE: &str = include_str!("../templates/ortho.tpl");

/// Which box face the projection looks through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// Along the box's X axis: image spans Y (width) and Z (height).
    X,
    /// Along Y: image spans Z (width) and X (height).
    Y,
    /// Along Z (top view for a Z-up scene): image spans X (width) and Y (height).
    Z,
}

impl Side {
    pub const ALL: [Side; 3] = [Side::Z, Side::X, Side::Y];

    pub fn label(self) -> &'static str {
        match self {
            Side::Z => "Top view (along Z)",
            Side::X => "Side view (along X)",
            Side::Y => "Side view (along Y)",
        }
    }

    /// RealityScan's `boxSideConerIndex` for this side, with `rotation` quarter turns.
    pub fn corner_index(self, rotation: u8) -> u8 {
        let base = match self {
            Side::X => 0,
            Side::Y => 8,
            Side::Z => 16,
        };
        base + (rotation % 4)
    }

    /// (width, height) of the projected face in box units.
    pub fn face_size(self, region: &Region) -> (f64, f64) {
        let [x, y, z] = region.size;
        match self {
            Side::X => (y, z),
            Side::Y => (z, x),
            Side::Z => (x, y),
        }
    }
}

/// Everything needed to write an `.rsortho`.
#[derive(Debug, Clone, PartialEq)]
pub struct OrthoRequest {
    pub name: String,
    pub model: String,
    pub side: Side,
    pub rotation: u8,
    /// Pixels along the longer face side.
    pub long_side_px: u32,
    /// Use the texture (`true`) or vertex colours.
    pub textured: bool,
    pub region: Region,
}

impl OrthoRequest {
    /// Pixel size preserving the face's aspect ratio.
    pub fn pixel_size(&self) -> (u32, u32) {
        let (w, h) = self.side.face_size(&self.region);
        let long = self.long_side_px.max(16) as f64;
        if w >= h {
            (long as u32, ((long * h / w).round() as u32).max(1))
        } else {
            (((long * w / h).round() as u32).max(1), long as u32)
        }
    }

    pub fn to_rsortho(&self) -> String {
        let (w, h) = self.pixel_size();
        let esc = |s: &str| s.replace('&', "&amp;").replace('"', "&quot;").replace('<', "&lt;");
        format!(
            concat!(
                "<OrthoProjection width=\"{w}\" height=\"{h}\" name=\"{name}\" modelName=\"{model}\" ",
                "colorType=\"{color}\" boxSideConerIndex=\"{idx}\" bEmpty=\"0\" backFaceColorType=\"1\" ",
                "backFaceColor=\"2130706687\">\n  <Header magic=\"5787472\" version=\"1\"/>\n</OrthoProjection>\n",
                "{region}",
                "<DTMParams classificationLayerId=\"-1\">\n",
                "  <ClassificationParams modelType=\"nature\" postprocessType=\"soft_edges\" sensitivity=\"0.5\"/>\n",
                "  <Header magic=\"1480868688\" version=\"1\"/>\n</DTMParams>\n"
            ),
            w = w,
            h = h,
            name = esc(&self.name),
            model = esc(&self.model),
            color = if self.textured { "texturing" } else { "coloring" },
            idx = self.side.corner_index(self.rotation),
            region = self.region.to_rsbox(),
        )
    }
}

/// Settings file for `exportOrthoProjection` (root `OrthoExport`, header magic 0, found by
/// trial against RealityScan 2.2; the attribute names come from its export library).
/// RealityScan appends `.<extension>` to the path it is given, so callers pass the path
/// without an extension and look for the result with [`ortho_output_candidates`].
pub fn export_xml(ext: &str) -> String {
    let ext = match ext.trim_start_matches('.').to_ascii_lowercase().as_str() {
        "tif" | "tiff" => "tiff",
        "jpg" | "jpeg" => "jpg",
        other => return export_xml_with(other),
    };
    export_xml_with(ext)
}

fn export_xml_with(ext: &str) -> String {
    format!(
        concat!(
            "<OrthoExport extension=\"{ext}\" pixelType=\"0\" compression=\"0\" ",
            "exportProjectionParametersFile=\"0\" exportWorldFile=\"0\" exportAsBigTiff=\"0\" ",
            "appendNameToFileName=\"0\" normalizeImage=\"0\" saveUndefinedToAlpha=\"1\" ",
            "worldFileCoordinateSystemType=\"0\">\n",
            "  <Header magic=\"0\" version=\"1\"/>\n",
            "</OrthoExport>\n"
        ),
        ext = ext,
    )
}

/// Files RealityScan may have written for an export requested as `path`: it appends the
/// extension itself (`foo.tiff` → `foo.tiff.tif` when given the full name, so the app passes
/// `foo` and gets `foo.tif`).
pub fn ortho_output_candidates(path: &std::path::Path) -> Vec<std::path::PathBuf> {
    let stem = path.with_extension("");
    let mut out = vec![path.to_path_buf()];
    for ext in ["tif", "tiff", "png", "jpg", "jpeg"] {
        out.push(stem.with_extension(ext));
        out.push(std::path::PathBuf::from(format!("{}.{ext}", path.display())));
    }
    out
}

/// An existing projection as the template reports it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrthoProjection {
    pub guid: String,
    pub name: String,
    pub width: u32,
    pub height: u32,
    /// Box extent along the viewing direction.
    pub depth: f64,
    pub box_width: f64,
    pub box_height: f64,
    #[serde(rename = "type")]
    pub kind: String,
}

pub fn parse_orthos(text: &str) -> Result<Vec<OrthoProjection>, Error> {
    let raw: Vec<Option<OrthoProjection>> = serde_json::from_str(&rs_client::json::sanitize(text))?;
    Ok(raw.into_iter().flatten().collect())
}

pub async fn fetch_orthos(client: &rs_client::Client) -> Result<Vec<OrthoProjection>, Error> {
    let text = client.template(ORTHO_TEMPLATE).await?;
    parse_orthos(&text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rsortho_keeps_aspect_and_embeds_region() {
        let req = OrthoRequest {
            name: "Top".into(),
            model: "Model 1".into(),
            side: Side::Z,
            rotation: 0,
            long_side_px: 2000,
            textured: true,
            region: Region::new([0.0; 3], [10.0, 5.0, 40.0], [0.0; 3]),
        };
        assert_eq!(req.pixel_size(), (2000, 1000));
        let xml = req.to_rsortho();
        assert!(xml.contains(r#"boxSideConerIndex="16""#));
        assert!(xml.contains(r#"colorType="texturing""#));
        assert!(xml.contains("<ReconstructionRegion"));
        assert!(xml.contains("<DTMParams"));
        let side = OrthoRequest { side: Side::X, ..req.clone() };
        assert_eq!(side.pixel_size(), (250, 2000));
    }

    #[test]
    fn parses_projection_list() {
        let text = r#"[{"guid":"{A}","name":"Ortho 0","width":1200,"height":1200,"depth":13.1,"boxWidth":13.0,"boxHeight":48.5,"type":"Arbitrary"},null]"#;
        let list = parse_orthos(text).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].box_height, 48.5);
        assert!(parse_orthos("[null]").unwrap().is_empty());
    }
}
