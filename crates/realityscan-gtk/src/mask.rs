//! A paintable image mask: full-resolution 8-bit data (255 = keep, 0 = masked out, which is
//! RealityScan's convention) plus a reduced-resolution red overlay texture for display.

use std::path::Path;

use gtk4 as gtk;

use gtk::prelude::*;
use gtk::{gdk, glib};

/// Longest side of the overlay texture.
const PREVIEW_MAX: u32 = 2048;

pub struct MaskLayer {
    pub width: u32,
    pub height: u32,
    /// Full-resolution mask, row-major, 255 = keep, 0 = masked out.
    pub data: Vec<u8>,
    preview_w: u32,
    preview_h: u32,
    /// RGBA overlay: red where masked out, transparent elsewhere.
    preview: Vec<u8>,
    pub dirty: bool,
    /// True once the user painted anything (or a mask was loaded).
    pub modified: bool,
}

impl MaskLayer {
    /// A fresh mask that keeps everything.
    pub fn new(width: u32, height: u32) -> Self {
        let scale = (PREVIEW_MAX as f64 / width.max(height) as f64).min(1.0);
        let preview_w = ((width as f64 * scale).round() as u32).max(1);
        let preview_h = ((height as f64 * scale).round() as u32).max(1);
        Self {
            width,
            height,
            data: vec![255; (width * height) as usize],
            preview_w,
            preview_h,
            preview: vec![0; (preview_w * preview_h * 4) as usize],
            dirty: true,
            modified: false,
        }
    }

    /// Load a mask image (any format `image` reads); resized to the given size if needed.
    pub fn load(path: &Path, width: u32, height: u32) -> Option<Self> {
        let img = image::ImageReader::open(path).ok()?.with_guessed_format().ok()?.decode().ok()?;
        let gray = if img.width() == width && img.height() == height {
            img.to_luma8()
        } else {
            image::imageops::resize(&img.to_luma8(), width, height, image::imageops::FilterType::Nearest)
        };
        let mut mask = Self::new(width, height);
        mask.data = gray.into_raw();
        mask.rebuild_preview();
        mask.modified = true;
        Some(mask)
    }

    /// Paint a disc of radius `r` (image pixels) at (`x`, `y`); `keep` = erase mask (white).
    pub fn paint(&mut self, x: f64, y: f64, r: f64, keep: bool) {
        let value = if keep { 255u8 } else { 0u8 };
        let (w, h) = (self.width as i64, self.height as i64);
        let (cx, cy, r) = (x.round() as i64, y.round() as i64, r.max(1.0));
        let ri = r.ceil() as i64;
        let r2 = r * r;
        for py in (cy - ri).max(0)..=(cy + ri).min(h - 1) {
            let dy = (py - cy) as f64;
            let half = (r2 - dy * dy).max(0.0).sqrt() as i64;
            let (x0, x1) = ((cx - half).max(0), (cx + half).min(w - 1));
            if x0 <= x1 {
                let row = (py * w) as usize;
                self.data[row + x0 as usize..=row + x1 as usize].fill(value);
            }
        }
        // Update the overlay for the touched area.
        let sx = self.preview_w as f64 / self.width as f64;
        let sy = self.preview_h as f64 / self.height as f64;
        let (px0, px1) = (
            (((cx - ri) as f64) * sx).floor().max(0.0) as u32,
            (((cx + ri) as f64) * sx).ceil().min(self.preview_w as f64 - 1.0) as u32,
        );
        let (py0, py1) = (
            (((cy - ri) as f64) * sy).floor().max(0.0) as u32,
            (((cy + ri) as f64) * sy).ceil().min(self.preview_h as f64 - 1.0) as u32,
        );
        for py in py0..=py1 {
            let iy = ((py as f64 + 0.5) / sy) as u32;
            for px in px0..=px1 {
                let ix = ((px as f64 + 0.5) / sx) as u32;
                let masked =
                    self.data[(iy.min(self.height - 1) * self.width + ix.min(self.width - 1)) as usize] < 128;
                let o = ((py * self.preview_w + px) * 4) as usize;
                self.preview[o..o + 4].copy_from_slice(if masked {
                    &[255, 40, 40, 120]
                } else {
                    &[0, 0, 0, 0]
                });
            }
        }
        self.dirty = true;
        self.modified = true;
    }

    fn rebuild_preview(&mut self) {
        let sx = self.width as f64 / self.preview_w as f64;
        let sy = self.height as f64 / self.preview_h as f64;
        for py in 0..self.preview_h {
            let iy = (((py as f64 + 0.5) * sy) as u32).min(self.height - 1);
            for px in 0..self.preview_w {
                let ix = (((px as f64 + 0.5) * sx) as u32).min(self.width - 1);
                let masked = self.data[(iy * self.width + ix) as usize] < 128;
                let o = ((py * self.preview_w + px) * 4) as usize;
                self.preview[o..o + 4].copy_from_slice(if masked {
                    &[255, 40, 40, 120]
                } else {
                    &[0, 0, 0, 0]
                });
            }
        }
        self.dirty = true;
    }

    /// Overlay texture (image-sized in logical terms; drawn stretched over the image).
    pub fn texture(&mut self) -> gdk::Texture {
        self.dirty = false;
        let bytes = glib::Bytes::from(&self.preview);
        gdk::MemoryTexture::new(
            self.preview_w as i32,
            self.preview_h as i32,
            gdk::MemoryFormat::R8g8b8a8,
            &bytes,
            (self.preview_w * 4) as usize,
        )
        .upcast()
    }

    /// Fraction of pixels masked out.
    pub fn masked_fraction(&self) -> f64 {
        let n = self.data.iter().filter(|&&v| v < 128).count();
        n as f64 / self.data.len().max(1) as f64
    }

    /// Write an 8-bit grayscale PNG.
    pub fn save_png(&self, path: &Path) -> Result<(), String> {
        let img =
            image::GrayImage::from_raw(self.width, self.height, self.data.clone()).ok_or("bad mask size")?;
        img.save(path).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paints_and_measures() {
        let mut m = MaskLayer::new(100, 50);
        assert_eq!(m.masked_fraction(), 0.0);
        m.paint(50.0, 25.0, 10.0, false);
        let f = m.masked_fraction();
        assert!(f > 0.05 && f < 0.08, "{f}");
        assert_eq!(m.data[25 * 100 + 50], 0);
        m.paint(50.0, 25.0, 12.0, true);
        assert_eq!(m.masked_fraction(), 0.0);
        assert!(m.modified);
    }
}
