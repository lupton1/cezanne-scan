//! Minimal ICC v2 display profiles, generated at run time so no third party profile
//! files need to be shipped. RGB uses sRGB primaries with a pure gamma curve.

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

fn build(class_space: &[u8; 4], tags: Vec<(&[u8; 4], Vec<u8>)>) -> Vec<u8> {
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
    h[12..16].copy_from_slice(b"mntr");
    h[16..20].copy_from_slice(class_space);
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
        b"GRAY",
        vec![
            (b"desc", desc_tag(&name)),
            (b"cprt", text_tag("No copyright, use freely")),
            (b"wtpt", xyz_tag(0.9642, 1.0, 0.8249)),
            (b"kTRC", curve_tag(gamma)),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_is_consistent() {
        for p in [rgb_profile(2.2), gray_profile(2.2)] {
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
}
