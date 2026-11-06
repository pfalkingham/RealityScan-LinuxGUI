//! Time decoding one JXL with jxl-oxide (full resolution) to size thumbnail work.
fn main() {
    let path = std::env::args().nth(1).expect("image path");
    let t = std::time::Instant::now();
    let file = std::io::BufReader::new(std::fs::File::open(&path).unwrap());
    let decoder = jxl_oxide::integration::JxlDecoder::new(file).unwrap();
    let img = image::DynamicImage::from_decoder(decoder).unwrap();
    println!("full decode {}x{} in {:.2?}", img.width(), img.height(), t.elapsed());
    let t = std::time::Instant::now();
    let thumb = img.thumbnail(320, 320);
    println!("thumbnail {}x{} in {:.2?}", thumb.width(), thumb.height(), t.elapsed());
}
