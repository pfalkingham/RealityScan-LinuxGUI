//! Binary glTF (`.glb`) reader for meshes exported by RealityScan.
//!
//! Reads every triangle primitive of every node in the default scene into one
//! [`Mesh`], keeping a [`Part`] per primitive so textured runs keep their
//! texture. Only what RealityScan writes is supported: embedded buffers, PNG or
//! JPEG images in buffer views (or `data:` URIs / files next to the `.glb`),
//! and the base colour texture of a PBR material.

use std::path::Path;

use serde::Deserialize;

use crate::mesh::{Mesh, MeshError, Part, Texture};

/// Textures larger than this are halved until they fit (box filter).
pub const MAX_TEXTURE_SIDE: u32 = 8192;

const MAGIC: u32 = 0x4654_6C67; // "glTF"
const CHUNK_JSON: u32 = 0x4E4F_534A;
const CHUNK_BIN: u32 = 0x004E_4942;

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct Gltf {
    #[serde(default)]
    scene: Option<usize>,
    #[serde(default)]
    scenes: Vec<SceneDef>,
    #[serde(default)]
    nodes: Vec<Node>,
    #[serde(default)]
    meshes: Vec<MeshDef>,
    #[serde(default)]
    accessors: Vec<Accessor>,
    #[serde(default)]
    buffer_views: Vec<BufferView>,
    #[serde(default)]
    buffers: Vec<Buffer>,
    #[serde(default)]
    materials: Vec<Material>,
    #[serde(default)]
    textures: Vec<TextureDef>,
    #[serde(default)]
    images: Vec<Image>,
}

#[derive(Deserialize, Default)]
struct SceneDef {
    #[serde(default)]
    nodes: Vec<usize>,
}

#[derive(Deserialize, Default)]
struct Node {
    #[serde(default)]
    children: Vec<usize>,
    mesh: Option<usize>,
    matrix: Option<[f32; 16]>,
    translation: Option<[f32; 3]>,
    rotation: Option<[f32; 4]>,
    scale: Option<[f32; 3]>,
}

#[derive(Deserialize, Default)]
struct MeshDef {
    #[serde(default)]
    primitives: Vec<Primitive>,
}

#[derive(Deserialize, Default)]
struct Primitive {
    #[serde(default)]
    attributes: std::collections::HashMap<String, usize>,
    indices: Option<usize>,
    material: Option<usize>,
    #[serde(default = "four")]
    mode: u32,
}

fn four() -> u32 {
    4
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct Accessor {
    buffer_view: Option<usize>,
    #[serde(default)]
    byte_offset: usize,
    component_type: u32,
    count: usize,
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    normalized: bool,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct BufferView {
    buffer: usize,
    #[serde(default)]
    byte_offset: usize,
    byte_length: usize,
    byte_stride: Option<usize>,
}

#[derive(Deserialize, Default)]
struct Buffer {
    uri: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct Material {
    #[serde(default)]
    pbr_metallic_roughness: Pbr,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct Pbr {
    base_color_texture: Option<TextureRef>,
}

#[derive(Deserialize, Default)]
struct TextureRef {
    index: usize,
}

#[derive(Deserialize, Default)]
struct TextureDef {
    source: Option<usize>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct Image {
    buffer_view: Option<usize>,
    mime_type: Option<String>,
    uri: Option<String>,
}

pub fn load_glb(path: &Path) -> Result<Mesh, MeshError> {
    parse_glb(&std::fs::read(path)?, path.parent())
}

/// Parse a `.glb` held in memory. `dir` resolves relative image/buffer URIs.
pub fn parse_glb(data: &[u8], dir: Option<&Path>) -> Result<Mesh, MeshError> {
    if data.len() < 12 || u32_at(data, 0) != MAGIC {
        return Err(MeshError::NotGlb);
    }
    let mut json: Option<&[u8]> = None;
    let mut bin: Option<&[u8]> = None;
    let mut pos = 12;
    while pos + 8 <= data.len() {
        let len = u32_at(data, pos) as usize;
        let kind = u32_at(data, pos + 4);
        let body = data.get(pos + 8..pos + 8 + len).ok_or_else(|| malformed("truncated chunk"))?;
        match kind {
            CHUNK_JSON => json = Some(body),
            CHUNK_BIN => bin = Some(body),
            _ => {}
        }
        pos += 8 + len;
    }
    let json = json.ok_or_else(|| malformed("no JSON chunk"))?;
    let doc: Gltf = serde_json::from_slice(json).map_err(|e| malformed(format!("glTF JSON: {e}")))?;

    let mut buffers: Vec<Vec<u8>> = Vec::with_capacity(doc.buffers.len());
    for (i, b) in doc.buffers.iter().enumerate() {
        buffers.push(match &b.uri {
            None if i == 0 => bin.ok_or_else(|| malformed("buffer 0 has no BIN chunk"))?.to_vec(),
            None => return Err(malformed(format!("buffer {i} has no data"))),
            Some(uri) => load_uri(uri, dir)?,
        });
    }

    let mut out = Out {
        mesh: Mesh::default(),
        images: Vec::new(),
        any_normals: false,
        any_uvs: false,
        any_colors: false,
    };
    out.images.resize_with(doc.images.len(), || None);
    let ctx = Ctx { doc: &doc, buffers: &buffers, dir };
    let roots: Vec<usize> = match doc.scene.or((!doc.scenes.is_empty()).then_some(0)) {
        Some(s) => doc.scenes.get(s).map(|s| s.nodes.clone()).unwrap_or_default(),
        None => (0..doc.nodes.len()).collect(),
    };
    for root in roots {
        visit(&ctx, &mut out, root, IDENTITY, 0)?;
    }

    let mut mesh = out.mesh;
    if !out.any_normals || mesh.normals.len() != mesh.positions.len() {
        mesh.compute_normals();
    }
    if !out.any_uvs {
        mesh.uvs.clear();
    }
    if !out.any_colors {
        mesh.colors.clear();
    }
    Ok(mesh)
}

struct Ctx<'a> {
    doc: &'a Gltf,
    buffers: &'a [Vec<u8>],
    dir: Option<&'a Path>,
}

struct Out {
    mesh: Mesh,
    /// Decoded images by glTF image index → index into `mesh.textures`.
    images: Vec<Option<Option<usize>>>,
    any_normals: bool,
    any_uvs: bool,
    any_colors: bool,
}

type Mat = [f32; 16]; // column-major

const IDENTITY: Mat = [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0];

fn visit(ctx: &Ctx, out: &mut Out, node_index: usize, parent: Mat, depth: usize) -> Result<(), MeshError> {
    if depth > 64 {
        return Err(malformed("node graph too deep"));
    }
    let node = ctx.doc.nodes.get(node_index).ok_or_else(|| malformed("bad node index"))?;
    let local = node_matrix(node);
    let world = mul(&parent, &local);
    if let Some(m) = node.mesh {
        let def = ctx.doc.meshes.get(m).ok_or_else(|| malformed("bad mesh index"))?;
        for prim in &def.primitives {
            add_primitive(ctx, out, prim, &world)?;
        }
    }
    for &c in &node.children {
        visit(ctx, out, c, world, depth + 1)?;
    }
    Ok(())
}

fn add_primitive(ctx: &Ctx, out: &mut Out, prim: &Primitive, world: &Mat) -> Result<(), MeshError> {
    if prim.mode != 4 {
        return Ok(());
    }
    let Some(&pos_acc) = prim.attributes.get("POSITION") else { return Ok(()) };
    let base = out.mesh.positions.len() as u32;
    let positions = read_f32s(ctx, pos_acc, 3)?;
    let count = positions.len() / 3;
    let is_identity = *world == IDENTITY;
    for p in positions.as_chunks::<3>().0 {
        let v = [p[0], p[1], p[2]];
        out.mesh.positions.push(if is_identity { v } else { transform_point(world, v) });
    }

    match prim.attributes.get("NORMAL") {
        Some(&acc) => {
            let n = read_f32s(ctx, acc, 3)?;
            if n.len() != count * 3 {
                return Err(malformed("NORMAL count mismatch"));
            }
            out.any_normals = true;
            out.mesh.normals.extend(n.as_chunks::<3>().0.iter().map(|n| {
                let v = [n[0], n[1], n[2]];
                if is_identity { v } else { normalize(transform_dir(world, v)) }
            }));
        }
        None => out.mesh.normals.extend(std::iter::repeat_n([0.0; 3], count)),
    }

    match prim.attributes.get("TEXCOORD_0") {
        Some(&acc) => {
            let uv = read_f32s(ctx, acc, 2)?;
            if uv.len() != count * 2 {
                return Err(malformed("TEXCOORD_0 count mismatch"));
            }
            out.any_uvs = true;
            out.mesh.uvs.extend(uv.as_chunks::<2>().0.iter().copied());
        }
        None => out.mesh.uvs.extend(std::iter::repeat_n([0.0; 2], count)),
    }

    match prim.attributes.get("COLOR_0") {
        Some(&acc) => {
            let acc_def = ctx.doc.accessors.get(acc).ok_or_else(|| malformed("bad accessor"))?;
            let comps = components(&acc_def.kind)?;
            let c = read_f32s(ctx, acc, comps)?;
            if c.len() != count * comps {
                return Err(malformed("COLOR_0 count mismatch"));
            }
            out.any_colors = true;
            out.mesh.colors.extend(c.chunks_exact(comps).map(|c| [to_u8(c[0]), to_u8(c[1]), to_u8(c[2])]));
        }
        None => out.mesh.colors.extend(std::iter::repeat_n([204u8; 3], count)),
    }

    let first_index = out.mesh.indices.len() as u32;
    match prim.indices {
        Some(acc) => {
            let idx = read_indices(ctx, acc)?;
            if idx.iter().any(|&i| i as usize >= count) {
                return Err(malformed("index out of range"));
            }
            out.mesh.indices.extend(idx.iter().map(|&i| base + i));
        }
        None => out.mesh.indices.extend((0..count as u32).map(|i| base + i)),
    }
    let index_count = out.mesh.indices.len() as u32 - first_index;

    let texture = match prim.material.and_then(|m| ctx.doc.materials.get(m)) {
        Some(mat) => match &mat.pbr_metallic_roughness.base_color_texture {
            Some(t) => texture_for(ctx, out, t.index)?,
            None => None,
        },
        None => None,
    };
    out.mesh.parts.push(Part { first_index, index_count, texture });
    Ok(())
}

/// Decode (once) the image behind glTF texture `index`; `None` when it can't be used.
fn texture_for(ctx: &Ctx, out: &mut Out, index: usize) -> Result<Option<usize>, MeshError> {
    let Some(source) = ctx.doc.textures.get(index).and_then(|t| t.source) else { return Ok(None) };
    let Some(slot) = out.images.get(source).copied() else { return Ok(None) };
    if let Some(done) = slot {
        return Ok(done);
    }
    let image = &ctx.doc.images[source];
    let bytes: Vec<u8> = match (&image.buffer_view, &image.uri) {
        (Some(bv), _) => view_bytes(ctx, *bv)?.to_vec(),
        (None, Some(uri)) => load_uri(uri, ctx.dir)?,
        (None, None) => return Ok(None),
    };
    let decoded = decode_image(&bytes, image.mime_type.as_deref());
    let result = match decoded {
        Ok(mut tex) => {
            tex.shrink_to(MAX_TEXTURE_SIDE);
            out.mesh.textures.push(tex);
            Some(out.mesh.textures.len() - 1)
        }
        Err(e) => {
            tracing::warn!("glTF image {source} not decodable: {e}");
            None
        }
    };
    out.images[source] = Some(result);
    Ok(result)
}

fn decode_image(bytes: &[u8], mime: Option<&str>) -> Result<Texture, image::ImageError> {
    let img = match mime {
        Some("image/png") => image::load_from_memory_with_format(bytes, image::ImageFormat::Png)?,
        Some("image/jpeg") => image::load_from_memory_with_format(bytes, image::ImageFormat::Jpeg)?,
        _ => image::load_from_memory(bytes)?,
    };
    let rgba = img.into_rgba8();
    let (width, height) = rgba.dimensions();
    Ok(Texture { width, height, rgba: rgba.into_raw() })
}

fn load_uri(uri: &str, dir: Option<&Path>) -> Result<Vec<u8>, MeshError> {
    if let Some(rest) = uri.strip_prefix("data:") {
        let payload = rest.split_once(";base64,").map(|(_, b)| b).ok_or_else(|| malformed("data URI"))?;
        return base64_decode(payload).ok_or_else(|| malformed("data URI base64"));
    }
    let path = dir.map_or_else(|| Path::new(uri).to_path_buf(), |d| d.join(uri.replace("%20", " ")));
    Ok(std::fs::read(path)?)
}

fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' | b'\n' | b'\r' | b' ' => continue,
            _ => return None,
        } as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

fn view_bytes<'a>(ctx: &Ctx<'a>, index: usize) -> Result<&'a [u8], MeshError> {
    let bv = ctx.doc.buffer_views.get(index).ok_or_else(|| malformed("bad bufferView"))?;
    let buf = ctx.buffers.get(bv.buffer).ok_or_else(|| malformed("bad buffer"))?;
    buf.get(bv.byte_offset..bv.byte_offset + bv.byte_length)
        .ok_or_else(|| malformed("bufferView out of range"))
}

fn components(kind: &str) -> Result<usize, MeshError> {
    Ok(match kind {
        "SCALAR" => 1,
        "VEC2" => 2,
        "VEC3" => 3,
        "VEC4" => 4,
        other => return Err(MeshError::Unsupported(format!("accessor type {other}"))),
    })
}

fn component_size(ct: u32) -> Result<usize, MeshError> {
    Ok(match ct {
        5120 | 5121 => 1,
        5122 | 5123 => 2,
        5125 | 5126 => 4,
        other => return Err(MeshError::Unsupported(format!("component type {other}"))),
    })
}

/// Read an accessor as floats, `want` components per element (extra components dropped,
/// missing ones an error). Normalised integer types are mapped to 0..1 / -1..1.
fn read_f32s(ctx: &Ctx, index: usize, want: usize) -> Result<Vec<f32>, MeshError> {
    let acc = ctx.doc.accessors.get(index).ok_or_else(|| malformed("bad accessor"))?;
    let comps = components(&acc.kind)?;
    if comps < want {
        return Err(malformed(format!("accessor {index}: {} has fewer than {want} components", acc.kind)));
    }
    let Some(bv_index) = acc.buffer_view else { return Ok(vec![0.0; acc.count * want]) };
    let bv = ctx.doc.buffer_views.get(bv_index).ok_or_else(|| malformed("bad bufferView"))?;
    let data = view_bytes(ctx, bv_index)?;
    let csize = component_size(acc.component_type)?;
    let stride = bv.byte_stride.unwrap_or(comps * csize);
    let mut out = Vec::with_capacity(acc.count * want);
    for i in 0..acc.count {
        let start = acc.byte_offset + i * stride;
        let el = data.get(start..start + comps * csize).ok_or_else(|| malformed("accessor out of range"))?;
        for c in 0..want {
            let b = &el[c * csize..(c + 1) * csize];
            out.push(match (acc.component_type, acc.normalized) {
                (5126, _) => f32::from_le_bytes([b[0], b[1], b[2], b[3]]),
                (5121, true) => b[0] as f32 / 255.0,
                (5121, false) => b[0] as f32,
                (5120, true) => (b[0] as i8 as f32 / 127.0).max(-1.0),
                (5120, false) => b[0] as i8 as f32,
                (5123, true) => u16::from_le_bytes([b[0], b[1]]) as f32 / 65535.0,
                (5123, false) => u16::from_le_bytes([b[0], b[1]]) as f32,
                (5122, true) => (i16::from_le_bytes([b[0], b[1]]) as f32 / 32767.0).max(-1.0),
                (5122, false) => i16::from_le_bytes([b[0], b[1]]) as f32,
                (5125, _) => u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f32,
                _ => unreachable!(),
            });
        }
    }
    Ok(out)
}

fn read_indices(ctx: &Ctx, index: usize) -> Result<Vec<u32>, MeshError> {
    let acc = ctx.doc.accessors.get(index).ok_or_else(|| malformed("bad accessor"))?;
    if acc.kind != "SCALAR" {
        return Err(malformed("indices must be SCALAR"));
    }
    let bv_index = acc.buffer_view.ok_or_else(|| malformed("indices without bufferView"))?;
    let bv = ctx.doc.buffer_views.get(bv_index).ok_or_else(|| malformed("bad bufferView"))?;
    let data = view_bytes(ctx, bv_index)?;
    let csize = component_size(acc.component_type)?;
    let stride = bv.byte_stride.unwrap_or(csize);
    let mut out = Vec::with_capacity(acc.count);
    for i in 0..acc.count {
        let start = acc.byte_offset + i * stride;
        let b = data.get(start..start + csize).ok_or_else(|| malformed("indices out of range"))?;
        out.push(match acc.component_type {
            5121 => b[0] as u32,
            5123 => u16::from_le_bytes([b[0], b[1]]) as u32,
            5125 => u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
            other => return Err(MeshError::Unsupported(format!("index component type {other}"))),
        });
    }
    Ok(out)
}

fn node_matrix(node: &Node) -> Mat {
    if let Some(m) = node.matrix {
        return m;
    }
    let t = node.translation.unwrap_or([0.0; 3]);
    let [qx, qy, qz, qw] = node.rotation.unwrap_or([0.0, 0.0, 0.0, 1.0]);
    let s = node.scale.unwrap_or([1.0; 3]);
    // Rotation matrix from the unit quaternion, columns scaled, translation last.
    let (xx, yy, zz) = (qx * qx, qy * qy, qz * qz);
    let (xy, xz, yz) = (qx * qy, qx * qz, qy * qz);
    let (wx, wy, wz) = (qw * qx, qw * qy, qw * qz);
    [
        (1.0 - 2.0 * (yy + zz)) * s[0],
        2.0 * (xy + wz) * s[0],
        2.0 * (xz - wy) * s[0],
        0.0,
        2.0 * (xy - wz) * s[1],
        (1.0 - 2.0 * (xx + zz)) * s[1],
        2.0 * (yz + wx) * s[1],
        0.0,
        2.0 * (xz + wy) * s[2],
        2.0 * (yz - wx) * s[2],
        (1.0 - 2.0 * (xx + yy)) * s[2],
        0.0,
        t[0],
        t[1],
        t[2],
        1.0,
    ]
}

fn mul(a: &Mat, b: &Mat) -> Mat {
    let mut m = [0.0; 16];
    for col in 0..4 {
        for row in 0..4 {
            m[col * 4 + row] = (0..4).map(|k| a[k * 4 + row] * b[col * 4 + k]).sum();
        }
    }
    m
}

fn transform_point(m: &Mat, p: [f32; 3]) -> [f32; 3] {
    [
        m[0] * p[0] + m[4] * p[1] + m[8] * p[2] + m[12],
        m[1] * p[0] + m[5] * p[1] + m[9] * p[2] + m[13],
        m[2] * p[0] + m[6] * p[1] + m[10] * p[2] + m[14],
    ]
}

fn transform_dir(m: &Mat, d: [f32; 3]) -> [f32; 3] {
    [
        m[0] * d[0] + m[4] * d[1] + m[8] * d[2],
        m[1] * d[0] + m[5] * d[1] + m[9] * d[2],
        m[2] * d[0] + m[6] * d[1] + m[10] * d[2],
    ]
}

fn normalize(v: [f32; 3]) -> [f32; 3] {
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if len > 0.0 { [v[0] / len, v[1] / len, v[2] / len] } else { v }
}

fn to_u8(v: f32) -> u8 {
    (v * 255.0).round().clamp(0.0, 255.0) as u8
}

fn u32_at(data: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]])
}

fn malformed(msg: impl Into<String>) -> MeshError {
    MeshError::Malformed(msg.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a one-triangle GLB with UVs and a 2×2 PNG texture, by hand.
    fn tiny_glb(with_texture: bool) -> Vec<u8> {
        let mut bin: Vec<u8> = Vec::new();
        let positions: [f32; 9] = [0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0];
        let uvs: [f32; 6] = [0.0, 0.0, 1.0, 0.0, 0.0, 1.0];
        let indices: [u16; 3] = [0, 1, 2];
        for v in positions {
            bin.extend_from_slice(&v.to_le_bytes());
        }
        let uv_off = bin.len();
        for v in uvs {
            bin.extend_from_slice(&v.to_le_bytes());
        }
        let idx_off = bin.len();
        for i in indices {
            bin.extend_from_slice(&i.to_le_bytes());
        }
        bin.extend_from_slice(&[0, 0]); // 4-byte alignment
        let png_off = bin.len();
        let mut png = Vec::new();
        let pixels = [255u8, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255];
        use image::ImageEncoder;
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(&pixels, 2, 2, image::ExtendedColorType::Rgba8)
            .unwrap();
        let png_len = png.len();
        bin.extend_from_slice(&png);
        while !bin.len().is_multiple_of(4) {
            bin.push(0);
        }
        let material = if with_texture {
            r#","materials":[{"pbrMetallicRoughness":{"baseColorTexture":{"index":0}}}],"textures":[{"source":0}],"images":[{"bufferView":3,"mimeType":"image/png"}]"#
        } else {
            ""
        };
        let json = format!(
            concat!(
                r#"{{"asset":{{"version":"2.0"}},"scene":0,"scenes":[{{"nodes":[0]}}],"#,
                r#""nodes":[{{"mesh":0,"translation":[10,0,0]}}],"#,
                r#""meshes":[{{"primitives":[{{"attributes":{{"POSITION":0,"TEXCOORD_0":1}},"indices":2{mat}}}]}}],"#,
                r#""accessors":[{{"bufferView":0,"componentType":5126,"count":3,"type":"VEC3"}},"#,
                r#"{{"bufferView":1,"componentType":5126,"count":3,"type":"VEC2"}},"#,
                r#"{{"bufferView":2,"componentType":5123,"count":3,"type":"SCALAR"}}],"#,
                r#""bufferViews":[{{"buffer":0,"byteOffset":0,"byteLength":36}},"#,
                r#"{{"buffer":0,"byteOffset":{uv_off},"byteLength":24}},"#,
                r#"{{"buffer":0,"byteOffset":{idx_off},"byteLength":6}},"#,
                r#"{{"buffer":0,"byteOffset":{png_off},"byteLength":{png_len}}}],"#,
                r#""buffers":[{{"byteLength":{bin_len}}}]{materials}}}"#
            ),
            mat = if with_texture { r#","material":0"# } else { "" },
            uv_off = uv_off,
            idx_off = idx_off,
            png_off = png_off,
            png_len = png_len,
            bin_len = bin.len(),
            materials = material,
        );
        let mut json = json.into_bytes();
        while json.len() % 4 != 0 {
            json.push(b' ');
        }
        let mut glb = Vec::new();
        glb.extend_from_slice(&MAGIC.to_le_bytes());
        glb.extend_from_slice(&2u32.to_le_bytes());
        glb.extend_from_slice(&((12 + 8 + json.len() + 8 + bin.len()) as u32).to_le_bytes());
        glb.extend_from_slice(&(json.len() as u32).to_le_bytes());
        glb.extend_from_slice(&CHUNK_JSON.to_le_bytes());
        glb.extend_from_slice(&json);
        glb.extend_from_slice(&(bin.len() as u32).to_le_bytes());
        glb.extend_from_slice(&CHUNK_BIN.to_le_bytes());
        glb.extend_from_slice(&bin);
        glb
    }

    #[test]
    fn parses_textured_triangle_with_node_transform() {
        let mesh = parse_glb(&tiny_glb(true), None).unwrap();
        assert_eq!(mesh.positions.len(), 3);
        assert_eq!(mesh.positions[1], [11.0, 0.0, 0.0]);
        assert_eq!(mesh.indices, vec![0, 1, 2]);
        assert_eq!(mesh.uvs[2], [0.0, 1.0]);
        assert!(mesh.has_normals());
        assert_eq!(mesh.normals[0], [0.0, 0.0, 1.0]);
        assert_eq!(mesh.parts.len(), 1);
        assert_eq!(mesh.parts[0].texture, Some(0));
        assert_eq!(mesh.textures[0].width, 2);
        assert_eq!(&mesh.textures[0].rgba[..4], &[255, 0, 0, 255]);
        assert!(mesh.is_textured());
        assert!(mesh.colors.is_empty());
    }

    #[test]
    fn untextured_primitive_has_no_texture() {
        let mesh = parse_glb(&tiny_glb(false), None).unwrap();
        assert_eq!(mesh.parts[0].texture, None);
        assert!(mesh.textures.is_empty());
        assert!(!mesh.is_textured());
    }

    #[test]
    fn rejects_other_data() {
        assert!(matches!(parse_glb(b"ply\n", None), Err(MeshError::NotGlb)));
    }

    #[test]
    fn shrink_halves_with_box_filter() {
        let mut t = Texture { width: 4, height: 2, rgba: (0..32).map(|i| (i * 8) as u8).collect() };
        t.shrink_to(2);
        assert_eq!((t.width, t.height), (2, 1));
        assert_eq!(t.rgba.len(), 8);
        assert_eq!(t.rgba[0], ((32 + 128 + 160 + 2) / 4) as u8);
    }

    #[test]
    fn base64_roundtrip() {
        assert_eq!(base64_decode("aGVsbG8=").unwrap(), b"hello");
        assert_eq!(base64_decode("aGk").unwrap(), b"hi");
    }
}
