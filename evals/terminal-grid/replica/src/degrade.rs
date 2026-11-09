//! What a Citrix session does to a frame on its way to the client: scaling,
//! a reduced colour depth and lossy compression. Applied to every frame the
//! replica draws, so an eval measures vision against the pixels it would get
//! through Citrix without needing Citrix.

use image::codecs::jpeg::JpegEncoder;
use image::imageops::{resize, FilterType};
use image::{ImageFormat, RgbImage};

#[derive(Debug, Default, Clone, Copy)]
pub struct Degrade {
    /// JPEG quality, 1-100.
    pub jpeg: Option<u8>,
    /// Shrink to this fraction and back, losing what the shrink loses.
    pub scale: Option<f32>,
    /// Colour depth: 16 (RGB565) or 8 (RGB332).
    pub bits: Option<u8>,
}

impl Degrade {
    pub fn is_clean(&self) -> bool {
        self.jpeg.is_none() && self.scale.is_none() && self.bits.is_none()
    }

    /// Degrade a top-down BGRA buffer in place.
    pub fn apply(&self, bgra: &mut [u8], width: u32, height: u32) -> Result<(), String> {
        let rgb: Vec<u8> = bgra
            .chunks_exact(4)
            .flat_map(|p| [p[2], p[1], p[0]])
            .collect();
        let mut img = RgbImage::from_raw(width, height, rgb).ok_or("frame size mismatch")?;
        if let Some(s) = self.scale {
            let w = ((width as f32 * s).round() as u32).max(1);
            let h = ((height as f32 * s).round() as u32).max(1);
            img = resize(
                &resize(&img, w, h, FilterType::Triangle),
                width,
                height,
                FilterType::Triangle,
            );
        }
        if let Some(bits) = self.bits {
            let mask = if bits <= 8 {
                [0xE0, 0xE0, 0xC0]
            } else {
                [0xF8, 0xFC, 0xF8]
            };
            for p in img.pixels_mut() {
                for (c, m) in p.0.iter_mut().zip(mask) {
                    *c &= m;
                }
            }
        }
        if let Some(q) = self.jpeg {
            let mut bytes = Vec::new();
            JpegEncoder::new_with_quality(&mut bytes, q.clamp(1, 100))
                .encode_image(&img)
                .map_err(|e| e.to_string())?;
            img = image::load_from_memory_with_format(&bytes, ImageFormat::Jpeg)
                .map_err(|e| e.to_string())?
                .to_rgb8();
        }
        for (out, p) in bgra.chunks_exact_mut(4).zip(img.pixels()) {
            out[..3].copy_from_slice(&[p[2], p[1], p[0]]);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sixteen_bit_colour_drops_the_low_bits() {
        let mut px = vec![0xFF, 0xFF, 0xFF, 0xFF, 0x07, 0x03, 0x07, 0xFF];
        Degrade {
            bits: Some(16),
            ..Default::default()
        }
        .apply(&mut px, 2, 1)
        .expect("applies");
        assert_eq!(px, vec![0xF8, 0xFC, 0xF8, 0xFF, 0x00, 0x00, 0x00, 0xFF]);
    }

    #[test]
    fn jpeg_keeps_a_flat_frame_close() {
        let mut px = [200u8, 100, 50, 255].repeat(64);
        Degrade {
            jpeg: Some(50),
            ..Default::default()
        }
        .apply(&mut px, 8, 8)
        .expect("applies");
        assert!(px
            .chunks_exact(4)
            .all(|p| p[0].abs_diff(200) < 8 && p[1].abs_diff(100) < 8));
    }
}
