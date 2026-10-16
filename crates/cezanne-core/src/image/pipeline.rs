//! Tone and colour pipeline.
//!
//! It follows the way Cezanne users work in ColorGeniusEX (see the forum notes in the
//! README): every channel has a highlight density (HD) and a shadow density (SD). A pixel
//! of density `d` maps to `(d - SD) / (HD - SD)`, so 0 is the shadow end and 1 the
//! highlight end. Entering HD above SD, as Cezanne users do for negatives, inverts the
//! image with no extra step, and per channel values remove the orange mask of colour
//! negatives. After that come a mid tone gamma, a master curve, output MIN and MAX levels
//! (0 to 255 like the original software), optional sharpening, greyscale and rotation.

use super::{Processed, RawImage};
use serde::{Deserialize, Serialize};

/// Optical density of a transmission value (0 to 1).
pub fn density(t: f64) -> f64 {
    -(t.max(1e-5)).log10()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FilmType {
    Positive,
    ColorNegative,
    BwNegative,
    Reflective,
}

impl FilmType {
    pub fn is_negative(self) -> bool {
        matches!(self, FilmType::ColorNegative | FilmType::BwNegative)
    }

    pub fn label(self) -> &'static str {
        match self {
            FilmType::Positive => "Positive (slide)",
            FilmType::ColorNegative => "Colour negative",
            FilmType::BwNegative => "Black and white negative",
            FilmType::Reflective => "Print (reflective)",
        }
    }

    pub const ALL: [FilmType; 4] = [FilmType::Positive, FilmType::ColorNegative, FilmType::BwNegative, FilmType::Reflective];

    /// The scanner's original type for this film (selects lamp and lookup tables).
    ///
    /// Negatives are scanned as positive film, the method Cezanne owners use with
    /// ColorGeniusEX: the scanner delivers plain linear transmission and the inversion and
    /// orange mask removal happen on the computer (flipped HD and SD). The scanner's own
    /// "Negative Film" type selects different lookup tables whose effect is not known yet.
    pub fn original(self) -> crate::protocol::OriginalType {
        use crate::protocol::OriginalType;
        match self {
            FilmType::Positive | FilmType::ColorNegative | FilmType::BwNegative => OriginalType::PositiveFilm,
            FilmType::Reflective => OriginalType::Photograph,
        }
    }
}

/// A tone curve through control points in 0..1, evaluated as a monotone cubic.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Curve {
    pub points: Vec<[f32; 2]>,
}

impl Default for Curve {
    fn default() -> Self {
        Curve::linear()
    }
}

impl Curve {
    pub fn linear() -> Curve {
        Curve { points: vec![[0.0, 0.0], [1.0, 1.0]] }
    }

    /// Gentle S curve, the usual starting point in the forum workflow.
    pub fn s_curve(strength: f32) -> Curve {
        let k = 0.06 * strength;
        Curve { points: vec![[0.0, 0.0], [0.25, 0.25 - k], [0.75, 0.75 + k], [1.0, 1.0]] }
    }

    pub fn is_linear(&self) -> bool {
        self.points.iter().all(|p| (p[0] - p[1]).abs() < 1e-6)
    }

    fn sorted(&self) -> Vec<(f64, f64)> {
        let mut p: Vec<(f64, f64)> =
            self.points.iter().map(|p| (p[0].clamp(0.0, 1.0) as f64, p[1].clamp(0.0, 1.0) as f64)).collect();
        p.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
        p.dedup_by(|a, b| (a.0 - b.0).abs() < 1e-9);
        if p.len() < 2 {
            return vec![(0.0, 0.0), (1.0, 1.0)];
        }
        p
    }

    /// Evaluate with Fritsch and Carlson monotone cubic interpolation.
    pub fn eval_many(&self, xs: &[f64]) -> Vec<f64> {
        let p = self.sorted();
        let n = p.len();
        let mut d = vec![0f64; n - 1];
        for i in 0..n - 1 {
            d[i] = (p[i + 1].1 - p[i].1) / (p[i + 1].0 - p[i].0);
        }
        let mut m = vec![0f64; n];
        m[0] = d[0];
        m[n - 1] = d[n - 2];
        for i in 1..n - 1 {
            m[i] = if d[i - 1] * d[i] <= 0.0 { 0.0 } else { (d[i - 1] + d[i]) / 2.0 };
        }
        for i in 0..n - 1 {
            if d[i] == 0.0 {
                m[i] = 0.0;
                m[i + 1] = 0.0;
            } else {
                let a = m[i] / d[i];
                let b = m[i + 1] / d[i];
                let s = a * a + b * b;
                if s > 9.0 {
                    let t = 3.0 / s.sqrt();
                    m[i] = t * a * d[i];
                    m[i + 1] = t * b * d[i];
                }
            }
        }
        xs.iter()
            .map(|&x| {
                if x <= p[0].0 {
                    return p[0].1;
                }
                if x >= p[n - 1].0 {
                    return p[n - 1].1;
                }
                let i = p.windows(2).position(|w| x >= w[0].0 && x <= w[1].0).unwrap_or(n - 2);
                let h = p[i + 1].0 - p[i].0;
                let t = (x - p[i].0) / h;
                let (t2, t3) = (t * t, t * t * t);
                let y = (2.0 * t3 - 3.0 * t2 + 1.0) * p[i].1
                    + (t3 - 2.0 * t2 + t) * h * m[i]
                    + (-2.0 * t3 + 3.0 * t2) * p[i + 1].1
                    + (t3 - t2) * h * m[i + 1];
                y.clamp(0.0, 1.0)
            })
            .collect()
    }

    pub fn eval(&self, x: f64) -> f64 {
        self.eval_many(&[x])[0]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Sharpen {
    pub enabled: bool,
    /// Strength, 0 to 3.
    pub amount: f32,
    /// Blur radius in pixels.
    pub radius: f32,
}

impl Default for Sharpen {
    fn default() -> Self {
        Sharpen { enabled: false, amount: 0.6, radius: 1.2 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GrayMode {
    /// Keep the scanner's channels.
    Keep,
    /// Weighted mix of R, G and B.
    Luminance,
    /// Keep one channel only (the forum tip for black and white negatives).
    Channel(u8),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Adjustments {
    pub film: FilmType,
    /// Highlight density per channel (R, G, B, grey uses R).
    pub hd: [f32; 3],
    /// Shadow density per channel.
    pub sd: [f32; 3],
    /// Mid tone gamma, 1 is neutral, above 1 brightens.
    pub gamma: f32,
    pub curve: Curve,
    /// Output levels on the 0 to 255 scale of the original software.
    pub output_min: f32,
    pub output_max: f32,
    pub sharpen: Sharpen,
    pub gray: GrayMode,
    /// Clockwise quarter turns.
    pub rotate: u8,
    pub mirror: bool,
}

impl Default for Adjustments {
    fn default() -> Self {
        Adjustments::for_film(FilmType::Positive)
    }
}

impl Adjustments {
    /// Starting values per film type, taken from the forum workflow.
    pub fn for_film(film: FilmType) -> Adjustments {
        let (hd, sd) = match film {
            FilmType::Positive | FilmType::Reflective => (0.15, 2.8),
            // flipped values invert the image (Corran's method): HD around 2.0 to 2.5, SD 0 to 0.2
            FilmType::ColorNegative | FilmType::BwNegative => (2.2, 0.15),
        };
        Adjustments {
            film,
            hd: [hd; 3],
            sd: [sd; 3],
            gamma: if film.is_negative() { 1.6 } else { 1.0 },
            curve: Curve::linear(),
            output_min: 0.0,
            output_max: 255.0,
            sharpen: Sharpen::default(),
            gray: if film == FilmType::BwNegative { GrayMode::Channel(1) } else { GrayMode::Keep },
            rotate: 0,
            mirror: false,
        }
    }

    /// Set HD and SD from the image itself, ignoring the outer `clip` fraction of pixels.
    /// For colour negatives this also removes the orange mask, as each channel is set
    /// separately.
    pub fn auto_levels(&mut self, img: &RawImage, clip: f64) {
        let c = img.channels;
        for ch in 0..c.min(3) {
            let mut hist = vec![0u32; 4096];
            for px in img.data.chunks_exact(c) {
                hist[(px[ch] >> 4) as usize] += 1;
            }
            let total: u64 = hist.iter().map(|&v| v as u64).sum();
            let pick = |q: f64| -> f64 {
                let target = (total as f64 * q) as u64;
                let mut acc = 0u64;
                for (i, &h) in hist.iter().enumerate() {
                    acc += h as u64;
                    if acc >= target {
                        return density((i as f64 + 0.5) * 16.0 / 65535.0);
                    }
                }
                density(1.0)
            };
            let thin = pick(1.0 - clip); // brightest pixels, lowest density
            let dense = pick(clip); // darkest pixels, highest density
            if self.film.is_negative() {
                self.hd[ch] = dense as f32;
                self.sd[ch] = thin as f32;
            } else {
                self.hd[ch] = thin as f32;
                self.sd[ch] = dense as f32;
            }
        }
        if c == 1 {
            self.hd = [self.hd[0]; 3];
            self.sd = [self.sd[0]; 3];
        }
    }

    /// Build a 4096 entry lookup table (input >> 4) per channel.
    fn luts(&self, channels: usize) -> Vec<Vec<u16>> {
        let xs: Vec<f64> = (0..4096)
            .map(|i| {
                // tone position before the curve, per channel computed below
                i as f64
            })
            .collect();
        let lo = self.output_min.clamp(0.0, 255.0) as f64 / 255.0;
        let hi = self.output_max.clamp(0.0, 255.0) as f64 / 255.0;
        let g = (self.gamma as f64).max(0.05);
        (0..channels)
            .map(|ch| {
                let hd = self.hd[ch.min(2)] as f64;
                let sd = self.sd[ch.min(2)] as f64;
                let span = if (hd - sd).abs() < 1e-4 { 1e-4 } else { hd - sd };
                let pos: Vec<f64> = xs
                    .iter()
                    .map(|&i| {
                        let t = (i + 0.5) * 16.0 / 65535.0;
                        let n = ((density(t) - sd) / span).clamp(0.0, 1.0);
                        n.powf(1.0 / g)
                    })
                    .collect();
                let curved = if self.curve.is_linear() { pos } else { self.curve.eval_many(&pos) };
                curved.iter().map(|&v| ((lo + v * (hi - lo)) * 65535.0).round().clamp(0.0, 65535.0) as u16).collect()
            })
            .collect()
    }

    /// Run the full pipeline.
    pub fn apply(&self, img: &RawImage) -> Processed {
        let c = img.channels;
        let luts = self.luts(c);
        let mut data: Vec<u16> = Vec::with_capacity(img.data.len());
        for px in img.data.chunks_exact(c) {
            for (ch, &v) in px.iter().enumerate() {
                data.push(luts[ch][(v >> 4) as usize]);
            }
        }
        let mut out = Processed { width: img.width, height: img.height, channels: c, x_dpi: img.x_dpi, y_dpi: img.y_dpi, data };

        if c == 3 {
            match self.gray {
                GrayMode::Keep => {}
                GrayMode::Luminance => to_gray(&mut out, |p| 0.2126 * p[0] as f64 + 0.7152 * p[1] as f64 + 0.0722 * p[2] as f64),
                GrayMode::Channel(k) => {
                    let k = (k as usize).min(2);
                    to_gray(&mut out, |p| p[k] as f64)
                }
            }
        }
        if self.sharpen.enabled && self.sharpen.amount > 0.0 {
            unsharp(&mut out, self.sharpen.radius.max(0.3), self.sharpen.amount);
        }
        if self.mirror {
            mirror(&mut out);
        }
        for _ in 0..(self.rotate % 4) {
            rotate_cw(&mut out);
        }
        out
    }
}

fn to_gray(img: &mut Processed, f: impl Fn(&[u16]) -> f64) {
    img.data = img.data.chunks_exact(3).map(|p| f(p).round().clamp(0.0, 65535.0) as u16).collect();
    img.channels = 1;
}

fn mirror(img: &mut Processed) {
    let (w, c) = (img.width as usize, img.channels);
    for row in img.data.chunks_exact_mut(w * c) {
        for x in 0..w / 2 {
            for k in 0..c {
                row.swap(x * c + k, (w - 1 - x) * c + k);
            }
        }
    }
}

fn rotate_cw(img: &mut Processed) {
    let (w, h, c) = (img.width as usize, img.height as usize, img.channels);
    let mut out = vec![0u16; img.data.len()];
    for y in 0..h {
        for x in 0..w {
            let nx = h - 1 - y;
            let ny = x;
            let src = (y * w + x) * c;
            let dst = (ny * h + nx) * c;
            out[dst..dst + c].copy_from_slice(&img.data[src..src + c]);
        }
    }
    img.data = out;
    std::mem::swap(&mut img.width, &mut img.height);
    std::mem::swap(&mut img.x_dpi, &mut img.y_dpi);
}

/// Unsharp mask with a three pass box blur approximating a Gaussian.
fn unsharp(img: &mut Processed, radius: f32, amount: f32) {
    let (w, h, c) = (img.width as usize, img.height as usize, img.channels);
    let r = ((radius * 0.8).round() as usize).max(1);
    for ch in 0..c {
        let src: Vec<f32> = img.data.iter().skip(ch).step_by(c).map(|&v| v as f32).collect();
        let mut blur = src.clone();
        let mut tmp = vec![0f32; w * h];
        for _ in 0..3 {
            box_h(&blur, &mut tmp, w, h, r);
            box_v(&tmp, &mut blur, w, h, r);
        }
        for (i, (&s, &b)) in src.iter().zip(&blur).enumerate() {
            let v = s + amount * (s - b);
            img.data[i * c + ch] = v.round().clamp(0.0, 65535.0) as u16;
        }
    }
}

fn box_h(src: &[f32], dst: &mut [f32], w: usize, h: usize, r: usize) {
    let norm = 1.0 / (2 * r + 1) as f32;
    for y in 0..h {
        let row = &src[y * w..(y + 1) * w];
        let mut acc: f32 = (0..=r).map(|i| row[i.min(w - 1)]).sum::<f32>() + r as f32 * row[0];
        for x in 0..w {
            dst[y * w + x] = acc * norm;
            let add = row[(x + r + 1).min(w - 1)];
            let sub = row[x.saturating_sub(r)];
            acc += add - sub;
        }
    }
}

fn box_v(src: &[f32], dst: &mut [f32], w: usize, h: usize, r: usize) {
    let norm = 1.0 / (2 * r + 1) as f32;
    for x in 0..w {
        let at = |y: usize| src[y.min(h - 1) * w + x];
        let mut acc: f32 = (0..=r).map(at).sum::<f32>() + r as f32 * at(0);
        for y in 0..h {
            dst[y * w + x] = acc * norm;
            acc += at(y + r + 1) - at(y.saturating_sub(r));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gray(values: &[u16]) -> RawImage {
        RawImage {
            width: values.len() as u32,
            height: 1,
            channels: 1,
            x_dpi: 100.0,
            y_dpi: 100.0,
            source_bits: 16,
            data: values.to_vec(),
        }
    }

    #[test]
    fn positive_keeps_bright_bright() {
        let a = Adjustments::for_film(FilmType::Positive);
        // density scale: 1.5 % transmission sits about a third of the way up
        let out = a.apply(&gray(&[65000, 1000, 100]));
        assert!(out.data[0] > 60000);
        assert!(out.data[1] < 30000 && out.data[1] > out.data[2]);
    }

    #[test]
    fn flipped_densities_invert_negatives() {
        let a = Adjustments::for_film(FilmType::BwNegative);
        // clear film base (thin) is a shadow, dense film is a highlight
        let out = a.apply(&gray(&[60000, 600]));
        assert!(out.data[0] < out.data[1]);
    }

    #[test]
    fn auto_levels_spans_the_range() {
        let vals: Vec<u16> = (0..1000).map(|i| 2000 + i * 50).collect();
        let img = gray(&vals);
        let mut a = Adjustments::for_film(FilmType::Positive);
        a.auto_levels(&img, 0.001);
        let out = a.apply(&img);
        let max = *out.data.iter().max().unwrap();
        let min = *out.data.iter().min().unwrap();
        assert!(max > 64000 && min < 1500, "min {min} max {max}");
    }

    #[test]
    fn output_levels_limit_the_range() {
        let mut a = Adjustments::for_film(FilmType::Positive);
        a.output_min = 10.0;
        a.output_max = 245.0;
        let out = a.apply(&gray(&[65535, 0]));
        assert!(out.data[0] <= (245.0f64 / 255.0 * 65535.0).round() as u16);
        assert!(out.data[1] >= (10.0f64 / 255.0 * 65535.0).round() as u16 - 1);
    }

    #[test]
    fn curve_is_monotone_and_hits_ends() {
        let c = Curve::s_curve(1.0);
        let ys = c.eval_many(&(0..=100).map(|i| i as f64 / 100.0).collect::<Vec<_>>());
        assert!(ys.windows(2).all(|w| w[1] >= w[0] - 1e-9));
        assert!(ys[0].abs() < 1e-9 && (ys[100] - 1.0).abs() < 1e-9);
        assert!(ys[25] < 0.25);
    }

    #[test]
    fn rotate_and_mirror() {
        let mut p = Processed { width: 2, height: 1, channels: 1, x_dpi: 1.0, y_dpi: 2.0, data: vec![1, 2] };
        rotate_cw(&mut p);
        assert_eq!((p.width, p.height), (1, 2));
        assert_eq!(p.data, vec![1, 2]);
        let mut q = Processed { width: 2, height: 1, channels: 1, x_dpi: 1.0, y_dpi: 1.0, data: vec![1, 2] };
        mirror(&mut q);
        assert_eq!(q.data, vec![2, 1]);
    }

    #[test]
    fn channel_pick_makes_grey() {
        let img = RawImage { width: 1, height: 1, channels: 3, x_dpi: 1.0, y_dpi: 1.0, source_bits: 16, data: vec![100, 50000, 9] };
        let mut a = Adjustments::for_film(FilmType::BwNegative);
        a.gray = GrayMode::Channel(1);
        let out = a.apply(&img);
        assert_eq!(out.channels, 1);
    }
}
