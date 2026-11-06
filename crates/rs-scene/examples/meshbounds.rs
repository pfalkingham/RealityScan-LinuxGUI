//! Print the bounds and first vertices of PLY/GLB meshes: `cargo run -p rs-scene --example meshbounds -- a.ply b.glb`
fn main() {
    for path in std::env::args().skip(1) {
        let p = std::path::Path::new(&path);
        let mesh = if p.extension().is_some_and(|e| e == "glb") {
            rs_scene::gltf::load_glb(p).expect("glb")
        } else {
            rs_scene::mesh::load_ply(p).expect("ply")
        };
        let (lo, hi) = mesh.bounds().unwrap();
        println!(
            "{}: {} tris, bounds {:.3?} .. {:.3?}, first vertex {:.3?}, uvs {} textures {}",
            p.file_name().unwrap().to_string_lossy(),
            mesh.triangle_count(),
            lo,
            hi,
            mesh.positions[0],
            mesh.has_uvs(),
            mesh.textures.len()
        );
    }
}
