//! OpenGL 3.3 renderer for the 3D viewport.
//!
//! Toolkit-agnostic: the caller owns the GL context (a `GtkGLArea` in the app),
//! hands over a [`glow::Context`], and calls [`Renderer::render`] from its draw
//! callback. Scene data arrives as flat vertex arrays via [`Renderer::set_points`],
//! [`Renderer::set_lines`] and [`Renderer::set_mesh`].

mod camera;
mod frustum;

pub use camera::OrbitCamera;
pub use frustum::{Pose, frustum_lines};

use glam::{Mat4, Vec3};
use glow::HasContext;

/// A coloured line segment in world space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Segment {
    pub a: Vec3,
    pub b: Vec3,
    pub color: [u8; 3],
}

/// A run of mesh triangles drawn with one texture (index into the uploaded textures).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MeshPart {
    pub first_index: u32,
    pub index_count: u32,
    pub texture: Option<usize>,
}

/// An RGBA8 image to upload as a mesh texture.
#[derive(Debug, Clone, Copy)]
pub struct TextureUpload<'a> {
    pub width: u32,
    pub height: u32,
    pub rgba: &'a [u8],
}

pub struct Renderer {
    gl: glow::Context,
    flat_program: glow::Program,
    lit_program: glow::Program,
    points: Buffer,
    lines: Buffer,
    mesh: Buffer,
    mesh_ebo: glow::Buffer,
    mesh_parts: Vec<MeshPart>,
    /// Indices of the highlighted (lasso-selected) triangles, drawn over the mesh.
    selection_ebo: glow::Buffer,
    selection_count: i32,
    textures: Vec<glow::Texture>,
    pub point_size: f32,
    pub show_points: bool,
    pub show_mesh: bool,
    /// Draw textures where a part has one (otherwise vertex colours).
    pub show_textures: bool,
    /// Draw vertex colours (otherwise a plain grey surface).
    pub show_colors: bool,
    pub background: [f32; 3],
}

struct Buffer {
    vao: glow::VertexArray,
    vbo: glow::Buffer,
    count: i32,
}

const FLAT_VERT: &str = r#"#version 330 core
layout(location = 0) in vec3 position;
layout(location = 1) in vec3 color;
uniform mat4 mvp;
uniform float point_size;
out vec3 v_color;
void main() {
    gl_Position = mvp * vec4(position, 1.0);
    gl_PointSize = point_size;
    v_color = color;
}
"#;

const FLAT_FRAG: &str = r#"#version 330 core
in vec3 v_color;
out vec4 frag;
void main() { frag = vec4(v_color, 1.0); }
"#;

const LIT_VERT: &str = r#"#version 330 core
layout(location = 0) in vec3 position;
layout(location = 1) in vec3 normal;
layout(location = 2) in vec3 color;
layout(location = 3) in vec2 uv;
uniform mat4 mvp;
uniform mat4 view;
out vec3 v_color;
out vec3 v_normal;
out vec2 v_uv;
void main() {
    gl_Position = mvp * vec4(position, 1.0);
    v_normal = mat3(view) * normal;
    v_color = color;
    v_uv = uv;
}
"#;

// Headlight shading: light comes from the viewer, so surfaces facing the camera are brightest.
const LIT_FRAG: &str = r#"#version 330 core
in vec3 v_color;
in vec3 v_normal;
in vec2 v_uv;
uniform sampler2D tex;
uniform float use_tex;
uniform vec3 tint;
uniform float tint_mix;
uniform float use_color;
out vec4 frag;
void main() {
    float d = abs(dot(normalize(v_normal), vec3(0.0, 0.0, 1.0)));
    vec3 surface = mix(vec3(0.8), v_color, use_color);
    vec3 base = mix(mix(surface, texture(tex, v_uv).rgb, use_tex), tint, tint_mix);
    frag = vec4(base * (0.25 + 0.75 * d), 1.0);
}
"#;

impl Renderer {
    /// Create GL resources. The context must be current.
    pub fn new(gl: glow::Context) -> Result<Self, String> {
        // SAFETY: GL calls on a context the caller made current.
        unsafe {
            let flat_program = compile(&gl, FLAT_VERT, FLAT_FRAG)?;
            let lit_program = compile(&gl, LIT_VERT, LIT_FRAG)?;
            let points = Buffer::new(&gl, &[3, 3])?;
            let lines = Buffer::new(&gl, &[3, 3])?;
            let mesh = Buffer::new(&gl, &[3, 3, 3, 2])?;
            let mesh_ebo = gl.create_buffer()?;
            let selection_ebo = gl.create_buffer()?;
            gl.enable(glow::PROGRAM_POINT_SIZE);
            gl.enable(glow::DEPTH_TEST);
            Ok(Self {
                gl,
                flat_program,
                lit_program,
                points,
                lines,
                mesh,
                mesh_ebo,
                mesh_parts: Vec::new(),
                selection_ebo,
                selection_count: 0,
                textures: Vec::new(),
                point_size: 2.0,
                show_points: true,
                show_mesh: true,
                show_textures: true,
                show_colors: true,
                background: [0.12, 0.12, 0.13],
            })
        }
    }

    /// Upload a point cloud: `positions` and `colors` must have equal length.
    pub fn set_points(&mut self, positions: &[Vec3], colors: &[[u8; 3]]) {
        let verts: Vec<f32> = positions
            .iter()
            .zip(colors)
            .flat_map(|(p, c)| [p.x, p.y, p.z, c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0])
            .collect();
        unsafe { self.points.upload(&self.gl, &verts, positions.len() as i32) };
    }

    pub fn set_lines(&mut self, segments: &[Segment]) {
        let verts: Vec<f32> = segments
            .iter()
            .flat_map(|s| {
                let c = [s.color[0] as f32 / 255.0, s.color[1] as f32 / 255.0, s.color[2] as f32 / 255.0];
                [s.a.x, s.a.y, s.a.z, c[0], c[1], c[2], s.b.x, s.b.y, s.b.z, c[0], c[1], c[2]]
            })
            .collect();
        unsafe { self.lines.upload(&self.gl, &verts, (segments.len() * 2) as i32) };
    }

    /// Upload an indexed triangle mesh. `normals` must match `positions`; `colors` and `uvs`
    /// may be empty (light grey / zero) or match `positions`. `parts` splits the index range
    /// by texture; empty draws everything untextured.
    pub fn set_mesh(
        &mut self,
        positions: &[[f32; 3]],
        normals: &[[f32; 3]],
        colors: &[[u8; 3]],
        uvs: &[[f32; 2]],
        indices: &[u32],
        parts: &[MeshPart],
    ) {
        let mut verts: Vec<f32> = Vec::with_capacity(positions.len() * 11);
        for (i, (p, n)) in positions.iter().zip(normals).enumerate() {
            let c = colors
                .get(i)
                .map_or([0.8, 0.8, 0.8], |c| [c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0]);
            let t = uvs.get(i).copied().unwrap_or([0.0, 0.0]);
            verts.extend_from_slice(&[p[0], p[1], p[2], n[0], n[1], n[2], c[0], c[1], c[2], t[0], t[1]]);
        }
        // SAFETY: GL calls on the caller's current context, with resources we created.
        unsafe {
            self.mesh.upload(&self.gl, &verts, indices.len() as i32);
            self.gl.bind_vertex_array(Some(self.mesh.vao));
            self.gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, Some(self.mesh_ebo));
            self.gl.buffer_data_u8_slice(glow::ELEMENT_ARRAY_BUFFER, as_bytes(indices), glow::STATIC_DRAW);
            self.gl.bind_vertex_array(None);
        }
        self.mesh_parts = parts.to_vec();
    }

    /// Replace the mesh textures (referenced by index from [`MeshPart::texture`]).
    pub fn set_textures(&mut self, images: &[TextureUpload<'_>]) {
        // SAFETY: GL calls on the caller's current context.
        unsafe {
            for t in self.textures.drain(..) {
                self.gl.delete_texture(t);
            }
            for img in images {
                let Ok(tex) = self.gl.create_texture() else { continue };
                self.gl.bind_texture(glow::TEXTURE_2D, Some(tex));
                self.gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_S, glow::REPEAT as i32);
                self.gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_T, glow::REPEAT as i32);
                self.gl.tex_parameter_i32(
                    glow::TEXTURE_2D,
                    glow::TEXTURE_MIN_FILTER,
                    glow::LINEAR_MIPMAP_LINEAR as i32,
                );
                self.gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, glow::LINEAR as i32);
                self.gl.tex_image_2d(
                    glow::TEXTURE_2D,
                    0,
                    glow::RGBA8 as i32,
                    img.width as i32,
                    img.height as i32,
                    0,
                    glow::RGBA,
                    glow::UNSIGNED_BYTE,
                    glow::PixelUnpackData::Slice(Some(img.rgba)),
                );
                self.gl.generate_mipmap(glow::TEXTURE_2D);
                self.textures.push(tex);
            }
            self.gl.bind_texture(glow::TEXTURE_2D, None);
        }
    }

    pub fn clear_mesh(&mut self) {
        self.mesh.count = 0;
        self.mesh_parts.clear();
        self.selection_count = 0;
        self.set_textures(&[]);
    }

    /// Highlight these triangles of the current mesh (three vertex indices each); empty clears.
    pub fn set_selection(&mut self, indices: &[u32]) {
        // SAFETY: GL calls on the caller's current context, with resources we created.
        unsafe {
            self.gl.bind_vertex_array(Some(self.mesh.vao));
            self.gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, Some(self.selection_ebo));
            self.gl.buffer_data_u8_slice(glow::ELEMENT_ARRAY_BUFFER, as_bytes(indices), glow::STATIC_DRAW);
            self.gl.bind_vertex_array(None);
        }
        self.selection_count = indices.len() as i32;
    }

    /// Depth-buffer value (0..1, 1 = background) at a device pixel, origin bottom-left.
    /// Only meaningful right after [`Renderer::render`], while its framebuffer is current.
    pub fn read_depth(&self, x: i32, y: i32) -> f32 {
        let mut px = [0f32; 1];
        // SAFETY: a 1×1 read into a buffer of the right size.
        unsafe {
            self.gl.read_pixels(
                x,
                y,
                1,
                1,
                glow::DEPTH_COMPONENT,
                glow::FLOAT,
                glow::PixelPackData::Slice(Some(as_bytes_mut(&mut px))),
            );
        }
        px[0]
    }

    /// The whole depth buffer, row-major from the bottom-left (see [`Renderer::read_depth`]).
    pub fn read_depth_buffer(&self, width: i32, height: i32) -> Vec<f32> {
        let mut buf = vec![0f32; (width.max(0) * height.max(0)) as usize];
        // SAFETY: the buffer holds width × height floats.
        unsafe {
            self.gl.read_pixels(
                0,
                0,
                width,
                height,
                glow::DEPTH_COMPONENT,
                glow::FLOAT,
                glow::PixelPackData::Slice(Some(as_bytes_mut(&mut buf))),
            );
        }
        buf
    }

    pub fn point_count(&self) -> usize {
        self.points.count as usize
    }

    pub fn mesh_triangle_count(&self) -> usize {
        self.mesh.count as usize / 3
    }

    /// Draw the scene into the current framebuffer.
    pub fn render(&self, width: i32, height: i32, camera: &OrbitCamera) {
        let aspect = if height > 0 { width as f32 / height as f32 } else { 1.0 };
        let view = camera.view();
        let mvp = camera.view_projection(aspect);
        let gl = &self.gl;
        // SAFETY: GL calls on the caller's current context, with resources we created.
        unsafe {
            gl.viewport(0, 0, width, height);
            gl.clear_color(self.background[0], self.background[1], self.background[2], 1.0);
            gl.clear(glow::COLOR_BUFFER_BIT | glow::DEPTH_BUFFER_BIT);

            if self.show_mesh && self.mesh.count > 0 {
                gl.use_program(Some(self.lit_program));
                set_mat4(gl, self.lit_program, "mvp", &mvp);
                set_mat4(gl, self.lit_program, "view", &view);
                gl.active_texture(glow::TEXTURE0);
                set_i32(gl, self.lit_program, "tex", 0);
                gl.bind_vertex_array(Some(self.mesh.vao));
                gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, Some(self.mesh_ebo));
                set_f32(gl, self.lit_program, "tint_mix", 0.0);
                set_f32(gl, self.lit_program, "use_color", if self.show_colors { 1.0 } else { 0.0 });
                if self.mesh_parts.is_empty() {
                    set_f32(gl, self.lit_program, "use_tex", 0.0);
                    gl.draw_elements(glow::TRIANGLES, self.mesh.count, glow::UNSIGNED_INT, 0);
                } else {
                    for part in &self.mesh_parts {
                        let tex = part.texture.and_then(|i| self.textures.get(i)).copied();
                        let textured = self.show_textures && tex.is_some();
                        gl.bind_texture(glow::TEXTURE_2D, if textured { tex } else { None });
                        set_f32(gl, self.lit_program, "use_tex", if textured { 1.0 } else { 0.0 });
                        gl.draw_elements(
                            glow::TRIANGLES,
                            part.index_count as i32,
                            glow::UNSIGNED_INT,
                            part.first_index as i32 * 4,
                        );
                    }
                    gl.bind_texture(glow::TEXTURE_2D, None);
                }
                if self.selection_count > 0 {
                    // The selected triangles again, tinted, nudged towards the viewer so they
                    // win the depth test against the copy underneath.
                    gl.enable(glow::POLYGON_OFFSET_FILL);
                    gl.polygon_offset(-1.0, -2.0);
                    set_f32(gl, self.lit_program, "use_tex", 0.0);
                    set_f32(gl, self.lit_program, "tint_mix", 0.8);
                    set_vec3(gl, self.lit_program, "tint", [1.0, 0.55, 0.1]);
                    gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, Some(self.selection_ebo));
                    gl.draw_elements(glow::TRIANGLES, self.selection_count, glow::UNSIGNED_INT, 0);
                    let err = gl.get_error();
                    if err != glow::NO_ERROR {
                        eprintln!(
                            "rs-render: selection pass GL error {err:#x} ({} indices)",
                            self.selection_count
                        );
                    }
                    gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, Some(self.mesh_ebo));
                    set_f32(gl, self.lit_program, "tint_mix", 0.0);
                    gl.disable(glow::POLYGON_OFFSET_FILL);
                }
            }
            if self.show_points && self.points.count > 0 {
                gl.use_program(Some(self.flat_program));
                set_mat4(gl, self.flat_program, "mvp", &mvp);
                set_f32(gl, self.flat_program, "point_size", self.point_size);
                gl.bind_vertex_array(Some(self.points.vao));
                gl.draw_arrays(glow::POINTS, 0, self.points.count);
            }
            if self.lines.count > 0 {
                gl.use_program(Some(self.flat_program));
                set_mat4(gl, self.flat_program, "mvp", &mvp);
                set_f32(gl, self.flat_program, "point_size", 1.0);
                gl.bind_vertex_array(Some(self.lines.vao));
                gl.draw_arrays(glow::LINES, 0, self.lines.count);
            }
            gl.bind_vertex_array(None);
            gl.use_program(None);
        }
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        // The context may already be gone when the widget is destroyed; GL ignores
        // deletes on a dead context, so this is best-effort.
        unsafe {
            self.gl.delete_program(self.flat_program);
            self.gl.delete_program(self.lit_program);
            self.gl.delete_buffer(self.mesh_ebo);
            self.gl.delete_buffer(self.selection_ebo);
            for t in self.textures.drain(..) {
                self.gl.delete_texture(t);
            }
            self.points.delete(&self.gl);
            self.lines.delete(&self.gl);
            self.mesh.delete(&self.gl);
        }
    }
}

impl Buffer {
    /// Interleaved float attributes with the given component counts, bound to locations 0, 1, …
    unsafe fn new(gl: &glow::Context, attribs: &[i32]) -> Result<Self, String> {
        unsafe {
            let vao = gl.create_vertex_array()?;
            let vbo = gl.create_buffer()?;
            gl.bind_vertex_array(Some(vao));
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
            let stride = attribs.iter().sum::<i32>() * std::mem::size_of::<f32>() as i32;
            let mut offset = 0;
            for (loc, &n) in attribs.iter().enumerate() {
                gl.enable_vertex_attrib_array(loc as u32);
                gl.vertex_attrib_pointer_f32(loc as u32, n, glow::FLOAT, false, stride, offset);
                offset += n * std::mem::size_of::<f32>() as i32;
            }
            gl.bind_vertex_array(None);
            Ok(Self { vao, vbo, count: 0 })
        }
    }

    unsafe fn upload(&mut self, gl: &glow::Context, verts: &[f32], count: i32) {
        unsafe {
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.vbo));
            gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, as_bytes(verts), glow::STATIC_DRAW);
            gl.bind_buffer(glow::ARRAY_BUFFER, None);
        }
        self.count = count;
    }

    unsafe fn delete(&self, gl: &glow::Context) {
        unsafe {
            gl.delete_vertex_array(self.vao);
            gl.delete_buffer(self.vbo);
        }
    }
}

fn as_bytes<T: Copy>(v: &[T]) -> &[u8] {
    // SAFETY: plain-old-data slices (f32/u32) have no padding and any bit pattern is a valid u8.
    unsafe { std::slice::from_raw_parts(v.as_ptr().cast::<u8>(), std::mem::size_of_val(v)) }
}

fn as_bytes_mut<T: Copy>(v: &mut [T]) -> &mut [u8] {
    // SAFETY: plain-data slice viewed as bytes, same lifetime and length in bytes.
    unsafe { std::slice::from_raw_parts_mut(v.as_mut_ptr() as *mut u8, std::mem::size_of_val(v)) }
}

unsafe fn set_vec3(gl: &glow::Context, program: glow::Program, name: &str, v: [f32; 3]) {
    // SAFETY: as for `set_mat4`.
    unsafe {
        let loc = gl.get_uniform_location(program, name);
        gl.uniform_3_f32(loc.as_ref(), v[0], v[1], v[2]);
    }
}

unsafe fn set_mat4(gl: &glow::Context, program: glow::Program, name: &str, m: &Mat4) {
    unsafe {
        let loc = gl.get_uniform_location(program, name);
        gl.uniform_matrix_4_f32_slice(loc.as_ref(), false, &m.to_cols_array());
    }
}

unsafe fn set_i32(gl: &glow::Context, program: glow::Program, name: &str, v: i32) {
    unsafe {
        let loc = gl.get_uniform_location(program, name);
        gl.uniform_1_i32(loc.as_ref(), v);
    }
}

unsafe fn set_f32(gl: &glow::Context, program: glow::Program, name: &str, v: f32) {
    unsafe {
        let loc = gl.get_uniform_location(program, name);
        gl.uniform_1_f32(loc.as_ref(), v);
    }
}

unsafe fn compile(gl: &glow::Context, vert: &str, frag: &str) -> Result<glow::Program, String> {
    unsafe {
        let program = gl.create_program()?;
        let mut shaders = Vec::new();
        for (kind, src) in [(glow::VERTEX_SHADER, vert), (glow::FRAGMENT_SHADER, frag)] {
            let shader = gl.create_shader(kind)?;
            gl.shader_source(shader, src);
            gl.compile_shader(shader);
            if !gl.get_shader_compile_status(shader) {
                return Err(format!("shader compile error: {}", gl.get_shader_info_log(shader)));
            }
            gl.attach_shader(program, shader);
            shaders.push(shader);
        }
        gl.link_program(program);
        if !gl.get_program_link_status(program) {
            return Err(format!("program link error: {}", gl.get_program_info_log(program)));
        }
        for shader in shaders {
            gl.detach_shader(program, shader);
            gl.delete_shader(shader);
        }
        Ok(program)
    }
}

/// Project a world point with a view-projection matrix to normalised device coordinates
/// (x, y in -1..1, depth in -1..1); `None` when it lies behind the eye.
/// Normalised device coordinates (x, y in -1..1, depth-buffer z in 0..1) back to world space.
pub fn unproject(view_projection: &Mat4, x: f32, y: f32, depth: f32) -> Option<Vec3> {
    let inv = view_projection.inverse();
    let clip = glam::Vec4::new(x, y, depth * 2.0 - 1.0, 1.0);
    let world = inv * clip;
    (world.w.abs() > 1e-12).then(|| world.truncate() / world.w)
}

pub fn project(view_projection: &Mat4, p: Vec3) -> Option<(f32, f32, f32)> {
    let clip = *view_projection * p.extend(1.0);
    (clip.w > 1e-6).then(|| (clip.x / clip.w, clip.y / clip.w, clip.z / clip.w))
}

/// Axis-aligned bounds of a point set, as (min, max).
pub fn bounds(points: &[Vec3]) -> Option<(Vec3, Vec3)> {
    let first = *points.first()?;
    Some(points.iter().fold((first, first), |(lo, hi), p| (lo.min(*p), hi.max(*p))))
}

/// Bounds after dropping the `trim` fraction of most extreme values on each axis
/// (0.05 ignores the lowest and highest 5 %), so stray points don't dominate framing.
pub fn robust_bounds(points: &[Vec3], trim: f32) -> Option<(Vec3, Vec3)> {
    let n = points.len();
    if n == 0 {
        return None;
    }
    let k = ((n as f32 * trim) as usize).min(n.saturating_sub(1) / 2);
    let (mut lo, mut hi) = (Vec3::ZERO, Vec3::ZERO);
    for axis in 0..3 {
        let mut v: Vec<f32> = points.iter().map(|p| p[axis]).collect();
        v.sort_by(f32::total_cmp);
        lo[axis] = v[k];
        hi[axis] = v[n - 1 - k];
    }
    Some((lo, hi))
}

/// World axes at `origin`, `len` long: X red, Y green, Z blue.
pub fn axes(origin: Vec3, len: f32) -> Vec<Segment> {
    vec![
        Segment { a: origin, b: origin + Vec3::X * len, color: [220, 60, 60] },
        Segment { a: origin, b: origin + Vec3::Y * len, color: [60, 200, 60] },
        Segment { a: origin, b: origin + Vec3::Z * len, color: [80, 120, 255] },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn robust_bounds_ignore_outliers() {
        let mut pts: Vec<Vec3> = (0..100).map(|i| Vec3::splat(i as f32 * 0.01)).collect();
        pts.push(Vec3::splat(1000.0));
        pts.push(Vec3::splat(-1000.0));
        let (lo, hi) = robust_bounds(&pts, 0.05).unwrap();
        assert!(lo.x > -1.0 && hi.x < 2.0, "{lo} {hi}");
        assert_eq!(robust_bounds(&[], 0.05), None);
        assert_eq!(robust_bounds(&[Vec3::ONE], 0.05), Some((Vec3::ONE, Vec3::ONE)));
    }
}
