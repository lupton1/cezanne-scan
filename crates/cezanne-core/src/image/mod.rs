//! Image data: raw scanner output, the adjustment pipeline and file output.

pub mod icc;
pub mod pipeline;
pub mod png;
pub mod tiff;

pub use pipeline::{Adjustments, Curve, FilmType};

use crate::error::{Error, Result};
use crate::protocol::Geometry;

/// Scanner output decoded to 16 bit samples, pixel interleaved.
///
/// Values are linear transmission (or reflectance): 0 is no light, 65535 is the
/// brightest the scanner measures through clear glass.
#[derive(Debug, Clone)]
pub struct RawImage {
    pub width: u32,
    pub height: u32,
    pub channels: usize,
    pub x_dpi: f64,
    pub y_dpi: f64,
    /// Bits per sample delivered by the scanner (8 or 16).
    pub source_bits: u8,
    pub data: Vec<u16>,
}

impl RawImage {
    pub fn from_scanner(g: &Geometry, bytes: &[u8], x_dpi: f64, y_dpi: f64) -> Result<RawImage> {
        if bytes.len() < g.total_bytes() {
            return Err(Error::Protocol(format!("expected {} image bytes, got {}", g.total_bytes(), bytes.len())));
        }
        let channels = g.mode.channels();
        let n = g.pixels_per_line as usize * g.lines as usize * channels;
        let data: Vec<u16> = if g.mode.bytes_per_sample() == 2 {
            bytes[..n * 2].chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect()
        } else {
            // 8 bit modes arrive gamma encoded, undo it so the pipeline always sees linear light
            let lut: Vec<u16> =
                (0..256).map(|v| ((v as f64 / 255.0).powf(2.2) * 65535.0).round() as u16).collect();
            bytes[..n].iter().map(|&v| lut[v as usize]).collect()
        };
        Ok(RawImage {
            width: g.pixels_per_line,
            height: g.lines,
            channels,
            x_dpi,
            y_dpi,
            source_bits: (g.mode.bytes_per_sample() * 8) as u8,
            data,
        })
    }

    pub fn pixel(&self, x: u32, y: u32) -> &[u16] {
        let i = (y as usize * self.width as usize + x as usize) * self.channels;
        &self.data[i..i + self.channels]
    }

    /// Mean of each channel, 0 to 1.
    pub fn channel_means(&self) -> Vec<f64> {
        let mut sum = vec![0f64; self.channels];
        for px in self.data.chunks_exact(self.channels) {
            for (s, &v) in sum.iter_mut().zip(px) {
                *s += v as f64;
            }
        }
        let n = (self.data.len() / self.channels).max(1) as f64;
        sum.iter().map(|s| s / n / 65535.0).collect()
    }

    /// Average density per channel over a rectangle (for the densitometer and auto levels).
    pub fn density_at(&self, x0: u32, y0: u32, w: u32, h: u32) -> Vec<f64> {
        let mut sum = vec![0f64; self.channels];
        let mut n = 0f64;
        for y in y0..(y0 + h).min(self.height) {
            for x in x0..(x0 + w).min(self.width) {
                for (s, &v) in sum.iter_mut().zip(self.pixel(x, y)) {
                    *s += v as f64;
                }
                n += 1.0;
            }
        }
        sum.iter().map(|s| pipeline::density((s / n.max(1.0)) / 65535.0)).collect()
    }

    /// Cut out a rectangle in pixel coordinates.
    pub fn crop(&self, x0: u32, y0: u32, w: u32, h: u32) -> RawImage {
        let x0 = x0.min(self.width.saturating_sub(1));
        let y0 = y0.min(self.height.saturating_sub(1));
        let w = w.min(self.width - x0).max(1);
        let h = h.min(self.height - y0).max(1);
        let mut data = Vec::with_capacity((w * h) as usize * self.channels);
        for y in y0..y0 + h {
            let start = (y as usize * self.width as usize + x0 as usize) * self.channels;
            data.extend_from_slice(&self.data[start..start + w as usize * self.channels]);
        }
        RawImage {
            width: w,
            height: h,
            channels: self.channels,
            x_dpi: self.x_dpi,
            y_dpi: self.y_dpi,
            source_bits: self.source_bits,
            data,
        }
    }

    /// Shrink by an integer factor (box filter), for previews.
    pub fn downsample(&self, factor: u32) -> RawImage {
        let f = factor.max(1);
        if f == 1 {
            return self.clone();
        }
        let w = (self.width / f).max(1);
        let h = (self.height / f).max(1);
        let c = self.channels;
        let mut data = vec![0u16; (w * h) as usize * c];
        for y in 0..h {
            for x in 0..w {
                let mut acc = vec![0u32; c];
                let mut n = 0u32;
                for yy in y * f..((y + 1) * f).min(self.height) {
                    for xx in x * f..((x + 1) * f).min(self.width) {
                        for (a, &v) in acc.iter_mut().zip(self.pixel(xx, yy)) {
                            *a += v as u32;
                        }
                        n += 1;
                    }
                }
                let o = (y * w + x) as usize * c;
                for (k, a) in acc.iter().enumerate() {
                    data[o + k] = (a / n.max(1)) as u16;
                }
            }
        }
        RawImage {
            width: w,
            height: h,
            channels: c,
            x_dpi: self.x_dpi / f as f64,
            y_dpi: self.y_dpi / f as f64,
            source_bits: self.source_bits,
            data,
        }
    }
}

/// A finished image ready to save: 16 bit, pixel interleaved, encoded for display.
#[derive(Debug, Clone)]
pub struct Processed {
    pub width: u32,
    pub height: u32,
    pub channels: usize,
    pub x_dpi: f64,
    pub y_dpi: f64,
    pub data: Vec<u16>,
}

impl Processed {
    /// 8 bit RGBA for on screen display.
    pub fn to_rgba8(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity((self.width * self.height) as usize * 4);
        for px in self.data.chunks_exact(self.channels) {
            let (r, g, b) = if self.channels == 3 { (px[0], px[1], px[2]) } else { (px[0], px[0], px[0]) };
            out.extend_from_slice(&[(r >> 8) as u8, (g >> 8) as u8, (b >> 8) as u8, 255]);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ColorMode;

    fn img() -> RawImage {
        let g = Geometry { pixels_per_line: 4, lines: 2, mode: ColorMode::Gray16 };
        let bytes: Vec<u8> = (0..8u16).flat_map(|v| (v * 1000).to_be_bytes()).collect();
        RawImage::from_scanner(&g, &bytes, 100.0, 100.0).unwrap()
    }

    #[test]
    fn decodes_big_endian() {
        let i = img();
        assert_eq!(i.pixel(1, 0), &[1000]);
        assert_eq!(i.pixel(3, 1), &[7000]);
    }

    #[test]
    fn crop_and_downsample() {
        let i = img();
        let c = i.crop(1, 0, 2, 2);
        assert_eq!((c.width, c.height), (2, 2));
        assert_eq!(c.pixel(0, 1), &[5000]);
        let d = i.downsample(2);
        assert_eq!((d.width, d.height), (2, 1));
        assert_eq!(d.pixel(0, 0), &[((0 + 1000 + 4000 + 5000) / 4) as u16]);
    }
}
