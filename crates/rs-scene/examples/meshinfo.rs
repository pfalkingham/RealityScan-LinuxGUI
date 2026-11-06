//! Load a PLY or GLB mesh and print what the parser made of it (with timings).
//!
//! cargo run -q -p rs-scene --example meshinfo -- model.glb

use std::time::Instant;

fn main() -> anyhow::Result<()> {
    let path = std::path::PathBuf::from(std::env::args().nth(1).expect("mesh path"));
    let t = Instant::now();
    let mesh = match path.extension().and_then(|e| e.to_str()) {
        Some("glb") => rs_scene::gltf::load_glb(&path)?,
        _ => rs_scene::mesh::load_ply(&path)?,
    };
    println!(
        "loaded in {:.2?}: {} vertices, {} triangles, normals={}, colors={}, uvs={}, parts={}, textures={:?}",
        t.elapsed(),
        mesh.positions.len(),
        mesh.triangle_count(),
        mesh.has_normals(),
        mesh.has_colors(),
        mesh.has_uvs(),
        mesh.parts.len(),
        mesh.textures
    );
    if let Some((lo, hi)) = mesh.bounds() {
        println!("bounds {lo:?} .. {hi:?}");
    }
    Ok(())
}
