//! Settings files for `exportSelectedModel` / `exportModel`.
//!
//! RealityScan takes an optional `params.xml` whose root is a `ModelExport` element;
//! the attribute names come from the `.rsInfo` file it writes next to an export.

/// Options for exporting a model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelExportOptions {
    pub vertex_colors: bool,
    pub vertex_normals: bool,
    pub textures: bool,
    /// Put textures inside the model file (formats that support it, e.g. glb).
    pub embed_textures: bool,
    /// Stitch all textures into one image.
    pub one_texture: bool,
    /// Write the `.rsInfo` file describing the coordinate system next to the export.
    pub info_file: bool,
    /// Binary rather than ASCII where the format offers both (ply, fbx).
    pub binary: bool,
}

impl Default for ModelExportOptions {
    fn default() -> Self {
        Self {
            vertex_colors: true,
            vertex_normals: true,
            textures: true,
            embed_textures: false,
            one_texture: false,
            info_file: false,
            binary: true,
        }
    }
}

impl ModelExportOptions {
    /// The settings XML for an export to a file with extension `ext` (`"obj"`, `"glb"`, …).
    ///
    /// RealityScan reads the format from `formatAndVersionUID`, not from the file name, and
    /// skips textures unless `exportedLayerCount` and a `Layer0` element are present, so the
    /// whole attribute set it writes itself is reproduced here.
    pub fn to_xml(&self, ext: &str) -> String {
        let b = |v: bool| if v { "1" } else { "0" };
        let ext = ext.trim_start_matches('.').to_ascii_lowercase();
        format!(
            concat!(
                "<ModelExport exportBinary=\"{binary}\" exportInfoFile=\"{info}\" exportVertices=\"1\" ",
                "exportVertexColors=\"{colors}\" exportVertexColorSpace=\"0\" exportVertexNormals=\"{normals}\" ",
                "exportTriangles=\"1\" exportTriangleStrips=\"0\" meshColor=\"1 1 1\" tileType=\"1\" ",
                "exportTextureAlpha=\"0\" exportToOneTexture=\"{one}\" embedTextures=\"{embed}\" ",
                "shrinkTextures=\"0\" oneTextureMaxSide=\"8192\" oneTextureUsePow2TexSide=\"1\" ",
                "exportCoordinateSystemType=\"0\" settingsAnchor=\"0 0 0\" settingsRotation=\"0 0 0\" ",
                "settingsScale=\"1 1 1\" normalSpace=\"2\" normalRange=\"0\" normalFlip=\"0 0 0\" ",
                "formatAndVersionUID=\"{ext} 000 \" exportModelByParts=\"0\" exportRandomPartColor=\"0\" ",
                "exportCameras=\"0\" exportCamerasAsModelPart=\"0\" exportMaterials=\"0\" ",
                "numberAsciiFormatting=\"5\" authorComment=\"\" exportedLayerCount=\"{layers}\" ",
                "classificationLayer=\"-1\" moreDetails=\"\">\n",
                "  <Header magic=\"5786949\" version=\"6\"/>\n",
                "  <Layer0 type=\"1\" textureLayerIndex=\"0\" ",
                "textureWicContainerFormat=\"{{1B7CFAF4-713F-473C-BBCD-6137425FAEAF}}\" ",
                "textureWicPixelFormat=\"{{6FDDC324-4E03-4BFE-B185-3D77768DC90F}}\" ",
                "textureExtension=\"png\" mapQualityToColor=\"0\"/>\n",
                "</ModelExport>\n"
            ),
            binary = b(self.binary),
            info = b(self.info_file),
            colors = b(self.vertex_colors),
            normals = b(self.vertex_normals),
            one = b(self.one_texture),
            embed = b(self.embed_textures),
            ext = ext,
            layers = if self.textures { 1 } else { 0 },
        )
    }
}

/// Model file formats RealityScan can write, with their extensions.
pub const MODEL_FORMATS: &[(&str, &str)] = &[
    ("Wavefront OBJ", "obj"),
    ("Polygon File Format (PLY)", "ply"),
    ("Binary glTF", "glb"),
    ("Autodesk FBX", "fbx"),
    ("Universal Scene Description (zipped)", "usdz"),
    ("Universal Scene Description", "usd"),
    ("Alembic", "abc"),
    ("Collada", "dae"),
    ("AutoCAD DXF", "dxf"),
    ("Stereolithography (STL)", "stl"),
    ("3D Manufacturing Format", "3mf"),
    ("XYZ point cloud", "xyz"),
    ("LAS point cloud", "las"),
    ("PTX point cloud", "ptx"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xml_carries_the_flags() {
        let xml = ModelExportOptions { embed_textures: true, vertex_normals: false, ..Default::default() }
            .to_xml(".GLB");
        assert!(xml.starts_with("<ModelExport "));
        assert!(xml.contains(r#"embedTextures="1""#));
        assert!(xml.contains(r#"exportVertexNormals="0""#));
        assert!(xml.contains(r#"formatAndVersionUID="glb 000 ""#));
        assert!(xml.contains(r#"exportedLayerCount="1""#));
        assert!(xml.contains(r#"<Header magic="5786949" version="6"/>"#));
        assert!(xml.contains("{1B7CFAF4-713F-473C-BBCD-6137425FAEAF}"));
        let plain = ModelExportOptions { textures: false, ..Default::default() }.to_xml("ply");
        assert!(plain.contains(r#"exportedLayerCount="0""#));
    }
}
