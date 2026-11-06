//! Triangle meshes exported from RealityScan, read from PLY files.
//!
//! Handles ASCII and binary little-endian PLY with arbitrary vertex properties
//! (position, optional normal and colour are picked out by name) and polygon
//! faces, which are fan-triangulated.

use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum MeshError {
    #[error("not a PLY file")]
    NotPly,
    #[error("not a binary glTF file")]
    NotGlb,
    #[error("unsupported PLY: {0}")]
    Unsupported(String),
    #[error("malformed PLY: {0}")]
    Malformed(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Mesh {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub colors: Vec<[u8; 3]>,
    /// Texture coordinates (glTF convention: origin top-left), empty when untextured.
    pub uvs: Vec<[f32; 2]>,
    /// Triangle list, three indices per triangle.
    pub indices: Vec<u32>,
    /// Index ranges with their texture; empty means one untextured part over all indices.
    pub parts: Vec<Part>,
    pub textures: Vec<Texture>,
}

/// A run of triangles sharing one texture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Part {
    pub first_index: u32,
    pub index_count: u32,
    /// Index into [`Mesh::textures`].
    pub texture: Option<usize>,
}

/// A decoded RGBA8 texture image.
#[derive(Clone, PartialEq, Eq)]
pub struct Texture {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl std::fmt::Debug for Texture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Texture({}x{})", self.width, self.height)
    }
}

impl Texture {
    /// Halve the image with a 2×2 box filter until neither side exceeds `max_side`.
    pub fn shrink_to(&mut self, max_side: u32) {
        while (self.width > max_side || self.height > max_side) && self.width > 1 && self.height > 1 {
            let (w, h) = (self.width as usize, self.height as usize);
            let (nw, nh) = (w / 2, h / 2);
            let mut out = vec![0u8; nw * nh * 4];
            for y in 0..nh {
                let r0 = &self.rgba[(2 * y) * w * 4..(2 * y + 1) * w * 4];
                let r1 = &self.rgba[(2 * y + 1) * w * 4..(2 * y + 2) * w * 4];
                let dst = &mut out[y * nw * 4..(y + 1) * nw * 4];
                for x in 0..nw {
                    for c in 0..4 {
                        let i = x * 8 + c;
                        let sum = r0[i] as u32 + r0[i + 4] as u32 + r1[i] as u32 + r1[i + 4] as u32;
                        dst[x * 4 + c] = ((sum + 2) / 4) as u8;
                    }
                }
            }
            self.width = nw as u32;
            self.height = nh as u32;
            self.rgba = out;
        }
    }
}

impl Mesh {
    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }

    pub fn has_normals(&self) -> bool {
        self.normals.len() == self.positions.len()
    }

    pub fn has_colors(&self) -> bool {
        self.colors.len() == self.positions.len()
    }

    pub fn has_uvs(&self) -> bool {
        !self.uvs.is_empty() && self.uvs.len() == self.positions.len()
    }

    pub fn is_textured(&self) -> bool {
        self.has_uvs() && self.parts.iter().any(|p| p.texture.is_some())
    }

    /// Area-weighted vertex normals from the faces (replaces any existing normals).
    pub fn compute_normals(&mut self) {
        let mut normals = vec![[0.0f32; 3]; self.positions.len()];
        for tri in self.indices.as_chunks::<3>().0 {
            let [a, b, c] = [tri[0] as usize, tri[1] as usize, tri[2] as usize];
            let (pa, pb, pc) = (self.positions[a], self.positions[b], self.positions[c]);
            let u = [pb[0] - pa[0], pb[1] - pa[1], pb[2] - pa[2]];
            let v = [pc[0] - pa[0], pc[1] - pa[1], pc[2] - pa[2]];
            let n = [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]];
            for i in [a, b, c] {
                for k in 0..3 {
                    normals[i][k] += n[k];
                }
            }
        }
        for n in &mut normals {
            let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
            if len > 0.0 {
                *n = [n[0] / len, n[1] / len, n[2] / len];
            }
        }
        self.normals = normals;
    }

    /// The triangles for which `keep[i]` is true, with the vertices they use compacted and the
    /// texture parts narrowed to what survives. Texture images are not carried over (the result
    /// is meant for export and re-import, where RealityScan textures afresh).
    pub fn subset(&self, keep: &[bool]) -> Mesh {
        let mut remap = vec![u32::MAX; self.positions.len()];
        let mut out = Mesh::default();
        let mut map = |old: u32, out: &mut Mesh| -> u32 {
            let slot = &mut remap[old as usize];
            if *slot == u32::MAX {
                *slot = out.positions.len() as u32;
                out.positions.push(self.positions[old as usize]);
                if self.has_normals() {
                    out.normals.push(self.normals[old as usize]);
                }
                if self.has_colors() {
                    out.colors.push(self.colors[old as usize]);
                }
                if self.has_uvs() {
                    out.uvs.push(self.uvs[old as usize]);
                }
            }
            *slot
        };
        let parts: Vec<Part> = if self.parts.is_empty() {
            vec![Part { first_index: 0, index_count: self.indices.len() as u32, texture: None }]
        } else {
            self.parts.clone()
        };
        for part in parts {
            let first_index = out.indices.len() as u32;
            let range = part.first_index as usize..(part.first_index + part.index_count) as usize;
            for (t, tri) in self.indices[range].as_chunks::<3>().0.iter().enumerate() {
                if keep.get(part.first_index as usize / 3 + t).copied().unwrap_or(false) {
                    for &v in tri {
                        let n = map(v, &mut out);
                        out.indices.push(n);
                    }
                }
            }
            let index_count = out.indices.len() as u32 - first_index;
            if index_count > 0 && !self.parts.is_empty() {
                out.parts.push(Part { first_index, index_count, texture: part.texture });
            }
        }
        out
    }

    pub fn bounds(&self) -> Option<([f32; 3], [f32; 3])> {
        let first = *self.positions.first()?;
        Some(self.positions.iter().fold((first, first), |(lo, hi), p| {
            (
                [lo[0].min(p[0]), lo[1].min(p[1]), lo[2].min(p[2])],
                [hi[0].max(p[0]), hi[1].max(p[1]), hi[2].max(p[2])],
            )
        }))
    }
}

/// Write a binary little-endian PLY (positions, normals and 8-bit colours when present, triangle
/// faces), the form RealityScan's `importModel` reads.
pub fn write_ply(mesh: &Mesh, path: &Path) -> std::io::Result<()> {
    use std::io::Write;
    let mut header = format!(
        "ply\nformat binary_little_endian 1.0\ncomment realityscan-gtk\nelement vertex {}\nproperty float x\nproperty float y\nproperty float z\n",
        mesh.positions.len()
    );
    if mesh.has_normals() {
        header.push_str("property float nx\nproperty float ny\nproperty float nz\n");
    }
    if mesh.has_colors() {
        header.push_str("property uchar red\nproperty uchar green\nproperty uchar blue\n");
    }
    header.push_str(&format!(
        "element face {}\nproperty list uchar int vertex_indices\nend_header\n",
        mesh.triangle_count()
    ));
    let mut out = std::io::BufWriter::with_capacity(1 << 20, std::fs::File::create(path)?);
    out.write_all(header.as_bytes())?;
    for (i, p) in mesh.positions.iter().enumerate() {
        for v in p {
            out.write_all(&v.to_le_bytes())?;
        }
        if mesh.has_normals() {
            for v in &mesh.normals[i] {
                out.write_all(&v.to_le_bytes())?;
            }
        }
        if mesh.has_colors() {
            out.write_all(&mesh.colors[i])?;
        }
    }
    for tri in mesh.indices.as_chunks::<3>().0 {
        out.write_all(&[3u8])?;
        for &v in tri {
            out.write_all(&(v as i32).to_le_bytes())?;
        }
    }
    out.flush()
}

pub fn load_ply(path: &Path) -> Result<Mesh, MeshError> {
    parse_ply(&std::fs::read(path)?)
}

#[derive(Clone, Copy, PartialEq)]
enum Format {
    Ascii,
    BinaryLe,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Scalar {
    I8,
    U8,
    I16,
    U16,
    I32,
    U32,
    F32,
    F64,
}

impl Scalar {
    fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "char" | "int8" => Self::I8,
            "uchar" | "uint8" => Self::U8,
            "short" | "int16" => Self::I16,
            "ushort" | "uint16" => Self::U16,
            "int" | "int32" => Self::I32,
            "uint" | "uint32" => Self::U32,
            "float" | "float32" => Self::F32,
            "double" | "float64" => Self::F64,
            _ => return None,
        })
    }

    fn size(self) -> usize {
        match self {
            Self::I8 | Self::U8 => 1,
            Self::I16 | Self::U16 => 2,
            Self::I32 | Self::U32 | Self::F32 => 4,
            Self::F64 => 8,
        }
    }

    fn read_le(self, b: &[u8]) -> f64 {
        match self {
            Self::I8 => b[0] as i8 as f64,
            Self::U8 => b[0] as f64,
            Self::I16 => i16::from_le_bytes([b[0], b[1]]) as f64,
            Self::U16 => u16::from_le_bytes([b[0], b[1]]) as f64,
            Self::I32 => i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64,
            Self::U32 => u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64,
            Self::F32 => f32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64,
            Self::F64 => f64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]),
        }
    }
}

#[derive(Clone, Debug)]
enum Property {
    Scalar { name: String, ty: Scalar },
    List { name: String, count: Scalar, item: Scalar },
}

struct Element {
    name: String,
    count: usize,
    props: Vec<Property>,
}

/// Parse a PLY file held in memory.
pub fn parse_ply(data: &[u8]) -> Result<Mesh, MeshError> {
    if !data.starts_with(b"ply") {
        return Err(MeshError::NotPly);
    }
    let header_end = find_header_end(data).ok_or(MeshError::Malformed("no end_header".into()))?;
    let header =
        std::str::from_utf8(&data[..header_end.0]).map_err(|_| MeshError::Malformed("header".into()))?;
    let mut format = None;
    let mut elements: Vec<Element> = Vec::new();
    for line in header.lines() {
        let mut it = line.split_whitespace();
        match it.next() {
            Some("format") => {
                format = Some(match it.next() {
                    Some("ascii") => Format::Ascii,
                    Some("binary_little_endian") => Format::BinaryLe,
                    other => return Err(MeshError::Unsupported(format!("format {other:?}"))),
                })
            }
            Some("element") => {
                let name = it.next().ok_or(MeshError::Malformed("element".into()))?.to_owned();
                let count = it
                    .next()
                    .and_then(|c| c.parse().ok())
                    .ok_or(MeshError::Malformed("element count".into()))?;
                elements.push(Element { name, count, props: Vec::new() });
            }
            Some("property") => {
                let el = elements.last_mut().ok_or(MeshError::Malformed("property before element".into()))?;
                let kind = it.next().ok_or(MeshError::Malformed("property".into()))?;
                let prop = if kind == "list" {
                    let count = Scalar::parse(it.next().unwrap_or(""))
                        .ok_or(MeshError::Unsupported("list count type".into()))?;
                    let item = Scalar::parse(it.next().unwrap_or(""))
                        .ok_or(MeshError::Unsupported("list item type".into()))?;
                    Property::List { name: it.next().unwrap_or("").to_owned(), count, item }
                } else {
                    let ty =
                        Scalar::parse(kind).ok_or_else(|| MeshError::Unsupported(format!("type {kind}")))?;
                    Property::Scalar { name: it.next().unwrap_or("").to_owned(), ty }
                };
                el.props.push(prop);
            }
            _ => {}
        }
    }
    let format = format.ok_or(MeshError::Malformed("no format line".into()))?;
    let body = &data[header_end.1..];
    match format {
        Format::BinaryLe => parse_binary(body, &elements),
        Format::Ascii => parse_ascii(body, &elements),
    }
}

/// (offset where the header text ends, offset where the body starts)
fn find_header_end(data: &[u8]) -> Option<(usize, usize)> {
    let needle = b"end_header";
    let pos = data.windows(needle.len()).position(|w| w == needle)?;
    let mut body = pos + needle.len();
    while body < data.len() && (data[body] == b'\r' || data[body] == b'\n') {
        body += 1;
        if data[body - 1] == b'\n' {
            break;
        }
    }
    Some((pos, body))
}

struct VertexLayout {
    /// Colours stored as floats are in 0..1 and need scaling to 0..255.
    float_colors: bool,
    x: usize,
    y: usize,
    z: usize,
    nx: Option<usize>,
    ny: Option<usize>,
    nz: Option<usize>,
    r: Option<usize>,
    g: Option<usize>,
    b: Option<usize>,
}

fn vertex_layout(props: &[Property]) -> Result<VertexLayout, MeshError> {
    let idx = |names: &[&str]| {
        props
            .iter()
            .position(|p| matches!(p, Property::Scalar { name, .. } if names.contains(&name.as_str())))
    };
    let r = idx(&["red", "r", "diffuse_red"]);
    let float_colors =
        r.is_some_and(|i| matches!(props[i], Property::Scalar { ty: Scalar::F32 | Scalar::F64, .. }));
    Ok(VertexLayout {
        float_colors,
        x: idx(&["x"]).ok_or(MeshError::Malformed("vertex x".into()))?,
        y: idx(&["y"]).ok_or(MeshError::Malformed("vertex y".into()))?,
        z: idx(&["z"]).ok_or(MeshError::Malformed("vertex z".into()))?,
        nx: idx(&["nx"]),
        ny: idx(&["ny"]),
        nz: idx(&["nz"]),
        r,
        g: idx(&["green", "g", "diffuse_green"]),
        b: idx(&["blue", "b", "diffuse_blue"]),
    })
}

fn push_vertex(mesh: &mut Mesh, layout: &VertexLayout, vals: &[f64], want_normals: bool, want_colors: bool) {
    mesh.positions.push([vals[layout.x] as f32, vals[layout.y] as f32, vals[layout.z] as f32]);
    if want_normals && let (Some(nx), Some(ny), Some(nz)) = (layout.nx, layout.ny, layout.nz) {
        mesh.normals.push([vals[nx] as f32, vals[ny] as f32, vals[nz] as f32]);
    }
    if want_colors && let (Some(r), Some(g), Some(b)) = (layout.r, layout.g, layout.b) {
        let scale = if layout.float_colors { 255.0 } else { 1.0 };
        let to_u8 = |v: f64| (v * scale).round().clamp(0.0, 255.0) as u8;
        mesh.colors.push([to_u8(vals[r]), to_u8(vals[g]), to_u8(vals[b])]);
    }
}

fn push_face(mesh: &mut Mesh, idx: &[u32]) {
    for i in 1..idx.len().saturating_sub(1) {
        mesh.indices.extend_from_slice(&[idx[0], idx[i], idx[i + 1]]);
    }
}

fn parse_binary(body: &[u8], elements: &[Element]) -> Result<Mesh, MeshError> {
    let mut mesh = Mesh::default();
    let mut pos = 0usize;
    let take = |pos: &mut usize, n: usize| -> Result<&[u8], MeshError> {
        let s = body.get(*pos..*pos + n).ok_or(MeshError::Malformed("truncated body".into()))?;
        *pos += n;
        Ok(s)
    };
    for el in elements {
        match el.name.as_str() {
            "vertex" => {
                let layout = vertex_layout(&el.props)?;
                let want_normals = layout.nx.is_some() && layout.ny.is_some() && layout.nz.is_some();
                let want_colors = layout.r.is_some() && layout.g.is_some() && layout.b.is_some();
                mesh.positions.reserve(el.count);
                let mut vals = vec![0f64; el.props.len()];
                for _ in 0..el.count {
                    for (i, p) in el.props.iter().enumerate() {
                        match p {
                            Property::Scalar { ty, .. } => vals[i] = ty.read_le(take(&mut pos, ty.size())?),
                            Property::List { count, item, .. } => {
                                let n = count.read_le(take(&mut pos, count.size())?) as usize;
                                take(&mut pos, n * item.size())?;
                            }
                        }
                    }
                    push_vertex(&mut mesh, &layout, &vals, want_normals, want_colors);
                }
            }
            "face" => {
                let mut idx = Vec::with_capacity(4);
                for _ in 0..el.count {
                    for p in &el.props {
                        match p {
                            Property::List { name, count, item } => {
                                let n = count.read_le(take(&mut pos, count.size())?) as usize;
                                if name.starts_with("vertex_ind") {
                                    idx.clear();
                                    for _ in 0..n {
                                        idx.push(item.read_le(take(&mut pos, item.size())?) as u32);
                                    }
                                    push_face(&mut mesh, &idx);
                                } else {
                                    take(&mut pos, n * item.size())?;
                                }
                            }
                            Property::Scalar { ty, .. } => {
                                take(&mut pos, ty.size())?;
                            }
                        }
                    }
                }
            }
            _ => {
                // Skip unknown elements (fixed-size only; lists make skipping impossible).
                for _ in 0..el.count {
                    for p in &el.props {
                        match p {
                            Property::Scalar { ty, .. } => {
                                take(&mut pos, ty.size())?;
                            }
                            Property::List { count, item, .. } => {
                                let n = count.read_le(take(&mut pos, count.size())?) as usize;
                                take(&mut pos, n * item.size())?;
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(mesh)
}

fn parse_ascii(body: &[u8], elements: &[Element]) -> Result<Mesh, MeshError> {
    let text = std::str::from_utf8(body).map_err(|_| MeshError::Malformed("ascii body".into()))?;
    let mut tokens = text.split_ascii_whitespace().map(|t| t.parse::<f64>());
    let mut next = || -> Result<f64, MeshError> {
        tokens
            .next()
            .ok_or(MeshError::Malformed("truncated body".into()))?
            .map_err(|_| MeshError::Malformed("number".into()))
    };
    let mut mesh = Mesh::default();
    for el in elements {
        match el.name.as_str() {
            "vertex" => {
                let layout = vertex_layout(&el.props)?;
                let want_normals = layout.nx.is_some() && layout.ny.is_some() && layout.nz.is_some();
                let want_colors = layout.r.is_some() && layout.g.is_some() && layout.b.is_some();
                let mut vals = vec![0f64; el.props.len()];
                for _ in 0..el.count {
                    for (i, p) in el.props.iter().enumerate() {
                        match p {
                            Property::Scalar { .. } => vals[i] = next()?,
                            Property::List { .. } => {
                                let n = next()? as usize;
                                for _ in 0..n {
                                    next()?;
                                }
                            }
                        }
                    }
                    push_vertex(&mut mesh, &layout, &vals, want_normals, want_colors);
                }
            }
            "face" => {
                let mut idx = Vec::with_capacity(4);
                for _ in 0..el.count {
                    for p in &el.props {
                        match p {
                            Property::List { name, .. } => {
                                let n = next()? as usize;
                                if name.starts_with("vertex_ind") {
                                    idx.clear();
                                    for _ in 0..n {
                                        idx.push(next()? as u32);
                                    }
                                    push_face(&mut mesh, &idx);
                                } else {
                                    for _ in 0..n {
                                        next()?;
                                    }
                                }
                            }
                            Property::Scalar { .. } => {
                                next()?;
                            }
                        }
                    }
                }
            }
            _ => {
                for _ in 0..el.count {
                    for p in &el.props {
                        match p {
                            Property::Scalar { .. } => {
                                next()?;
                            }
                            Property::List { .. } => {
                                let n = next()? as usize;
                                for _ in 0..n {
                                    next()?;
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(mesh)
}

#[cfg(test)]
mod tests {
    fn two_triangles() -> Mesh {
        Mesh {
            positions: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [1.0, 1.0, 0.0]],
            normals: vec![[0.0, 0.0, 1.0]; 4],
            colors: vec![[10, 20, 30], [40, 50, 60], [70, 80, 90], [100, 110, 120]],
            uvs: Vec::new(),
            indices: vec![0, 1, 2, 1, 3, 2],
            parts: Vec::new(),
            textures: Vec::new(),
        }
    }

    #[test]
    fn subset_compacts_vertices() {
        let mesh = two_triangles();
        let kept = mesh.subset(&[false, true]);
        assert_eq!(kept.triangle_count(), 1);
        assert_eq!(kept.positions.len(), 3, "only the vertices the kept triangle uses");
        assert_eq!(kept.indices, vec![0, 1, 2]);
        assert_eq!(kept.positions, vec![[1.0, 0.0, 0.0], [1.0, 1.0, 0.0], [0.0, 1.0, 0.0]]);
        assert_eq!(kept.colors, vec![[40, 50, 60], [100, 110, 120], [70, 80, 90]]);
        assert!(kept.has_normals() && kept.parts.is_empty());
        assert_eq!(mesh.subset(&[false, false]).positions.len(), 0);
    }

    #[test]
    fn subset_keeps_texture_parts() {
        let mut mesh = two_triangles();
        mesh.uvs = vec![[0.0, 0.0]; 4];
        mesh.parts = vec![
            Part { first_index: 0, index_count: 3, texture: Some(0) },
            Part { first_index: 3, index_count: 3, texture: Some(1) },
        ];
        let kept = mesh.subset(&[true, true]);
        assert_eq!(kept.parts.len(), 2);
        assert_eq!(kept.parts[1], Part { first_index: 3, index_count: 3, texture: Some(1) });
        let kept = mesh.subset(&[false, true]);
        assert_eq!(kept.parts, vec![Part { first_index: 0, index_count: 3, texture: Some(1) }]);
    }

    #[test]
    fn ply_round_trip() {
        let mesh = two_triangles();
        let path = std::env::temp_dir().join(format!("rsgtk-roundtrip-{}.ply", std::process::id()));
        write_ply(&mesh, &path).unwrap();
        let back = load_ply(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(back.positions, mesh.positions);
        assert_eq!(back.indices, mesh.indices);
        assert_eq!(back.colors, mesh.colors);
        assert!(back.has_normals());
    }

    use super::*;

    const ASCII: &str = "ply\nformat ascii 1.0\ncomment test\nelement vertex 4\nproperty float x\nproperty float y\nproperty float z\nproperty uchar red\nproperty uchar green\nproperty uchar blue\nelement face 1\nproperty list uchar int vertex_indices\nend_header\n0 0 0 255 0 0\n1 0 0 0 255 0\n1 1 0 0 0 255\n0 1 0 9 9 9\n4 0 1 2 3\n";

    #[test]
    fn parses_ascii_quad_as_two_triangles() {
        let mesh = parse_ply(ASCII.as_bytes()).unwrap();
        assert_eq!(mesh.positions.len(), 4);
        assert_eq!(mesh.colors[2], [0, 0, 255]);
        assert_eq!(mesh.indices, vec![0, 1, 2, 0, 2, 3]);
        assert_eq!(mesh.triangle_count(), 2);
    }

    #[test]
    fn parses_binary_little_endian() {
        let mut data = b"ply\nformat binary_little_endian 1.0\nelement vertex 3\nproperty float x\nproperty float y\nproperty float z\nproperty float nx\nproperty float ny\nproperty float nz\nelement face 1\nproperty list uchar uint vertex_indices\nend_header\n".to_vec();
        for v in [[0f32, 0., 0., 0., 0., 1.], [1., 0., 0., 0., 0., 1.], [0., 1., 0., 0., 0., 1.]] {
            for f in v {
                data.extend_from_slice(&f.to_le_bytes());
            }
        }
        data.push(3);
        for i in [0u32, 1, 2] {
            data.extend_from_slice(&i.to_le_bytes());
        }
        let mesh = parse_ply(&data).unwrap();
        assert_eq!(mesh.positions, vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]]);
        assert!(mesh.has_normals());
        assert_eq!(mesh.indices, vec![0, 1, 2]);
        assert!(!mesh.has_colors());
    }

    #[test]
    fn float_colours_are_scaled() {
        let ply = "ply\nformat ascii 1.0\nelement vertex 1\nproperty float x\nproperty float y\nproperty float z\nproperty float red\nproperty float green\nproperty float blue\nelement face 0\nproperty list uchar int vertex_indices\nend_header\n0 0 0 1.0 0.5 0\n";
        let mesh = parse_ply(ply.as_bytes()).unwrap();
        assert_eq!(mesh.colors, vec![[255, 128, 0]]);
    }

    #[test]
    fn computes_normals() {
        let mut mesh = parse_ply(ASCII.as_bytes()).unwrap();
        mesh.compute_normals();
        assert!(mesh.has_normals());
        for n in &mesh.normals {
            assert!((n[2] - 1.0).abs() < 1e-6, "{n:?}");
        }
    }

    #[test]
    fn rejects_other_files() {
        assert!(matches!(parse_ply(b"OFF\n"), Err(MeshError::NotPly)));
        assert!(matches!(
            parse_ply(b"ply\nformat binary_big_endian 1.0\nend_header\n"),
            Err(MeshError::Unsupported(_))
        ));
    }
}
