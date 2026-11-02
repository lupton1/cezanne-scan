//! Minimal ICC v2 profiles, generated at run time so no third party profile files need to
//! be shipped.
//!
//! Finished scans are tagged with the scanner's own colour space, the one ColorGeniusEX
//! used. Its profile for the FT-S5000 is SCREEN's "5000stdRGB V1.1" (input class, matrix
//! and gamma), and the owner's saved ColorGeniusEX recipes use it as both the input and the
//! output profile, so the RGB data was never converted to another space. [`output_profile`]
//! rebuilds a profile with exactly the same primaries, white point and gamma.

/// Colorimetry of SCREEN's 5000stdRGB V1.1, as the raw s15Fixed16 numbers in that profile.
pub mod ft_s5000 {
    pub const WHITE: [i32; 3] = [63190, 65536, 54060];
    pub const RED: [i32; 3] = [36890, 15502, 329];
    pub const GREEN: [i32; 3] = [18668, 48455, 3555];
    pub const BLUE: [i32; 3] = [7619, 1577, 50206];
    /// Tone curve gamma as stored (u8Fixed8): 586 / 256 = 2.289.
    pub const GAMMA_U8F8: u16 = 586;
}

/// Gamma of the scanner colour space, 2.289.
pub const SCANNER_GAMMA: f64 = ft_s5000::GAMMA_U8F8 as f64 / 256.0;

fn fixed(v: [i32; 3]) -> [f64; 3] {
    v.map(|x| x as f64 / 65536.0)
}

/// sRGB primaries adapted to D50, as in [`rgb_profile`].
const SRGB_D50: [[f64; 3]; 3] = [[0.4361, 0.2225, 0.0139], [0.3851, 0.7169, 0.0971], [0.1431, 0.0606, 0.7141]];

fn s15(v: f64) -> [u8; 4] {
    ((v * 65536.0).round() as i32).to_be_bytes()
}

fn xyz_tag(x: f64, y: f64, z: f64) -> Vec<u8> {
    let mut t = b"XYZ \0\0\0\0".to_vec();
    t.extend_from_slice(&s15(x));
    t.extend_from_slice(&s15(y));
    t.extend_from_slice(&s15(z));
    t
}

fn curve_tag(gamma: f64) -> Vec<u8> {
    let mut t = b"curv\0\0\0\0".to_vec();
    t.extend_from_slice(&1u32.to_be_bytes());
    t.extend_from_slice(&((gamma * 256.0).round() as u16).to_be_bytes());
    t
}

fn desc_tag(s: &str) -> Vec<u8> {
    let mut t = b"desc\0\0\0\0".to_vec();
    t.extend_from_slice(&((s.len() + 1) as u32).to_be_bytes());
    t.extend_from_slice(s.as_bytes());
    t.push(0);
    t.extend_from_slice(&[0; 8]); // unicode language and count
    t.extend_from_slice(&[0; 3]); // scriptcode code and count
    t.extend_from_slice(&[0; 67]);
    t
}

fn text_tag(s: &str) -> Vec<u8> {
    let mut t = b"text\0\0\0\0".to_vec();
    t.extend_from_slice(s.as_bytes());
    t.push(0);
    t
}

fn build(class: &[u8; 4], space: &[u8; 4], tags: Vec<(&[u8; 4], Vec<u8>)>) -> Vec<u8> {
    let table_len = 4 + 12 * tags.len();
    let mut offset = 128 + table_len;
    let mut table = Vec::new();
    let mut body = Vec::new();
    table.extend_from_slice(&(tags.len() as u32).to_be_bytes());
    for (sig, data) in &tags {
        while offset % 4 != 0 {
            body.push(0);
            offset += 1;
        }
        table.extend_from_slice(*sig);
        table.extend_from_slice(&(offset as u32).to_be_bytes());
        table.extend_from_slice(&(data.len() as u32).to_be_bytes());
        body.extend_from_slice(data);
        offset += data.len();
    }
    while offset % 4 != 0 {
        body.push(0);
        offset += 1;
    }
    let mut h = vec![0u8; 128];
    h[0..4].copy_from_slice(&(offset as u32).to_be_bytes());
    h[8..12].copy_from_slice(&[0x02, 0x10, 0, 0]);
    h[12..16].copy_from_slice(class);
    h[16..20].copy_from_slice(space);
    h[20..24].copy_from_slice(b"XYZ ");
    h[24..36].copy_from_slice(&[0x07, 0xEA, 0, 10, 0, 15, 0, 0, 0, 0, 0, 0]); // 2026-10-15
    h[36..40].copy_from_slice(b"acsp");
    h[68..72].copy_from_slice(&s15(0.9642));
    h[72..76].copy_from_slice(&s15(1.0));
    h[76..80].copy_from_slice(&s15(0.8249));
    let mut p = h;
    p.extend_from_slice(&table);
    p.extend_from_slice(&body);
    p
}

/// RGB profile: sRGB primaries (D50 adapted), D50 white, pure gamma.
pub fn rgb_profile(gamma: f64) -> Vec<u8> {
    let name = format!("Cezanne Scan RGB gamma {gamma:.1}");
    build(
        b"mntr",
        b"RGB ",
        vec![
            (b"desc", desc_tag(&name)),
            (b"cprt", text_tag("No copyright, use freely")),
            (b"wtpt", xyz_tag(0.9642, 1.0, 0.8249)),
            (b"rXYZ", xyz_tag(0.4361, 0.2225, 0.0139)),
            (b"gXYZ", xyz_tag(0.3851, 0.7169, 0.0971)),
            (b"bXYZ", xyz_tag(0.1431, 0.0606, 0.7141)),
            (b"rTRC", curve_tag(gamma)),
            (b"gTRC", curve_tag(gamma)),
            (b"bTRC", curve_tag(gamma)),
        ],
    )
}

/// Greyscale profile with a pure gamma curve.
pub fn gray_profile(gamma: f64) -> Vec<u8> {
    let name = format!("Cezanne Scan Grey gamma {gamma:.1}");
    build(
        b"mntr",
        b"GRAY",
        vec![
            (b"desc", desc_tag(&name)),
            (b"cprt", text_tag("No copyright, use freely")),
            (b"wtpt", xyz_tag(0.9642, 1.0, 0.8249)),
            (b"kTRC", curve_tag(gamma)),
        ],
    )
}

/// RGB in the FT-S5000's colour space (SCREEN's 5000stdRGB colorimetry) with the given
/// gamma. 1.0 describes raw linear scanner data.
pub fn scanner_rgb_profile(gamma: f64) -> Vec<u8> {
    let w = fixed(ft_s5000::WHITE);
    let [r, g, b] = [fixed(ft_s5000::RED), fixed(ft_s5000::GREEN), fixed(ft_s5000::BLUE)];
    let name = if (gamma - SCANNER_GAMMA).abs() < 1e-6 {
        "FT-S5000 RGB (SCREEN 5000stdRGB V1.1)".to_string()
    } else {
        format!("FT-S5000 RGB gamma {gamma:.1}")
    };
    build(
        b"scnr",
        b"RGB ",
        vec![
            (b"desc", desc_tag(&name)),
            (b"cprt", text_tag("Made by Cezanne Scan, use freely")),
            (b"wtpt", xyz_tag(w[0], w[1], w[2])),
            (b"rXYZ", xyz_tag(r[0], r[1], r[2])),
            (b"gXYZ", xyz_tag(g[0], g[1], g[2])),
            (b"bXYZ", xyz_tag(b[0], b[1], b[2])),
            (b"rTRC", curve_tag(gamma)),
            (b"gTRC", curve_tag(gamma)),
            (b"bTRC", curve_tag(gamma)),
        ],
    )
}

/// Greyscale with the scanner's tone curve.
pub fn scanner_gray_profile(gamma: f64) -> Vec<u8> {
    let w = fixed(ft_s5000::WHITE);
    build(
        b"scnr",
        b"GRAY",
        vec![
            (b"desc", desc_tag(&format!("FT-S5000 Grey gamma {gamma:.3}"))),
            (b"cprt", text_tag("Made by Cezanne Scan, use freely")),
            (b"wtpt", xyz_tag(w[0], w[1], w[2])),
            (b"kTRC", curve_tag(gamma)),
        ],
    )
}

/// The profile embedded in finished scans, as ColorGeniusEX did.
pub fn output_profile(channels: usize) -> Vec<u8> {
    if channels == 3 {
        scanner_rgb_profile(SCANNER_GAMMA)
    } else {
        scanner_gray_profile(SCANNER_GAMMA)
    }
}

/// Luminance weights of the scanner colour space (the Y of each primary).
pub fn scanner_luminance() -> [f64; 3] {
    [fixed(ft_s5000::RED)[1], fixed(ft_s5000::GREEN)[1], fixed(ft_s5000::BLUE)[1]]
}

/// Matrix from linear scanner RGB to linear sRGB, both relative to D50, for showing
/// finished images on screen the way a colour managed editor shows the saved file.
pub fn scanner_to_srgb() -> [[f64; 3]; 3] {
    let cols = |m: [[f64; 3]; 3]| [[m[0][0], m[1][0], m[2][0]], [m[0][1], m[1][1], m[2][1]], [m[0][2], m[1][2], m[2][2]]];
    let src = cols([fixed(ft_s5000::RED), fixed(ft_s5000::GREEN), fixed(ft_s5000::BLUE)]);
    let dst = invert(cols(SRGB_D50));
    let mut m = [[0.0; 3]; 3];
    for (i, row) in m.iter_mut().enumerate() {
        for (j, v) in row.iter_mut().enumerate() {
            *v = (0..3).map(|k| dst[i][k] * src[k][j]).sum();
        }
    }
    m
}

fn invert(m: [[f64; 3]; 3]) -> [[f64; 3]; 3] {
    let c = |r: usize, k: usize| m[(r + 1) % 3][(k + 1) % 3] * m[(r + 2) % 3][(k + 2) % 3] - m[(r + 1) % 3][(k + 2) % 3] * m[(r + 2) % 3][(k + 1) % 3];
    let det = m[0][0] * c(0, 0) + m[0][1] * c(0, 1) + m[0][2] * c(0, 2);
    let mut o = [[0.0; 3]; 3];
    for (i, row) in o.iter_mut().enumerate() {
        for (j, v) in row.iter_mut().enumerate() {
            *v = c(j, i) / det;
        }
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_is_consistent() {
        for p in [rgb_profile(2.2), gray_profile(2.2), output_profile(3), output_profile(1), scanner_rgb_profile(1.0)] {
            assert_eq!(u32::from_be_bytes([p[0], p[1], p[2], p[3]]) as usize, p.len());
            assert_eq!(&p[36..40], b"acsp");
            assert_eq!(p.len() % 4, 0);
            let count = u32::from_be_bytes([p[128], p[129], p[130], p[131]]) as usize;
            for i in 0..count {
                let e = 132 + i * 12;
                let off = u32::from_be_bytes([p[e + 4], p[e + 5], p[e + 6], p[e + 7]]) as usize;
                let len = u32::from_be_bytes([p[e + 8], p[e + 9], p[e + 10], p[e + 11]]) as usize;
                assert!(off % 4 == 0 && off + len <= p.len());
            }
        }
    }

    fn tag<'a>(p: &'a [u8], sig: &[u8; 4]) -> &'a [u8] {
        let count = u32::from_be_bytes([p[128], p[129], p[130], p[131]]) as usize;
        (0..count)
            .map(|i| 132 + i * 12)
            .find(|&e| &p[e..e + 4] == sig)
            .map(|e| {
                let off = u32::from_be_bytes([p[e + 4], p[e + 5], p[e + 6], p[e + 7]]) as usize;
                let len = u32::from_be_bytes([p[e + 8], p[e + 9], p[e + 10], p[e + 11]]) as usize;
                &p[off..off + len]
            })
            .expect("tag present")
    }

    fn xyz(t: &[u8]) -> [i32; 3] {
        [0, 1, 2].map(|k| i32::from_be_bytes([t[8 + 4 * k], t[9 + 4 * k], t[10 + 4 * k], t[11 + 4 * k]]))
    }

    #[test]
    fn output_profile_matches_screen_5000stdrgb() {
        let p = output_profile(3);
        assert_eq!(&p[12..20], b"scnrRGB ");
        assert_eq!(xyz(tag(&p, b"wtpt")), ft_s5000::WHITE);
        assert_eq!(xyz(tag(&p, b"rXYZ")), ft_s5000::RED);
        assert_eq!(xyz(tag(&p, b"gXYZ")), ft_s5000::GREEN);
        assert_eq!(xyz(tag(&p, b"bXYZ")), ft_s5000::BLUE);
        for t in [b"rTRC", b"gTRC", b"bTRC"] {
            let c = tag(&p, t);
            assert_eq!(u16::from_be_bytes([c[12], c[13]]), 586);
        }
        let g = output_profile(1);
        assert_eq!(&g[12..20], b"scnrGRAY");
        let k = tag(&g, b"kTRC");
        assert_eq!(u16::from_be_bytes([k[12], k[13]]), 586);
    }

    #[test]
    fn white_stays_white_on_screen() {
        let m = scanner_to_srgb();
        for row in m {
            let sum: f64 = row.iter().sum();
            assert!((sum - 1.0).abs() < 0.01, "{row:?}");
        }
        // the scanner's red is wider than sRGB, so it goes out of gamut on screen
        assert!(m[1][0] < 0.0 || m[2][0] < 0.0);
    }
}
