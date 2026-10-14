//! Baseline TIFF writer: uncompressed, chunky, 8 or 16 bits per sample, with resolution
//! tags and an optional embedded ICC profile.

use super::Processed;
use crate::error::{Error, Result};
use std::io::{BufWriter, Seek, Write};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum BitDepth {
    Eight,
    Sixteen,
}

pub fn write_tiff(path: &Path, img: &Processed, depth: BitDepth, icc: Option<&[u8]>) -> Result<()> {
    let f = std::fs::File::create(path)?;
    let mut w = BufWriter::with_capacity(1 << 20, f);
    encode(&mut w, img, depth, icc)?;
    w.flush()?;
    Ok(())
}

pub fn encode<W: Write + Seek>(w: &mut W, img: &Processed, depth: BitDepth, icc: Option<&[u8]>) -> Result<()> {
    let spp = img.channels as u32;
    if spp != 1 && spp != 3 {
        return Err(Error::InvalidParameter("TIFF output needs 1 or 3 channels".into()));
    }
    let bps: u32 = if depth == BitDepth::Sixteen { 16 } else { 8 };
    let row_bytes = img.width as u64 * spp as u64 * (bps / 8) as u64;
    let rows_per_strip = ((1u64 << 20) / row_bytes.max(1)).clamp(1, img.height as u64) as u32;
    let strips = img.height.div_ceil(rows_per_strip);
    let image_bytes = row_bytes * img.height as u64;
    if 8 + image_bytes + 4096 + icc.map_or(0, |p| p.len() as u64) > u32::MAX as u64 {
        return Err(Error::InvalidParameter("image too large for a classic TIFF (over 4 GB)".into()));
    }

    // layout: header | image strips | extra data | IFD
    w.write_all(b"II*\0")?;
    w.write_all(&[0; 4])?; // first IFD offset, patched at the end
    let mut extra: Vec<u8> = Vec::new();
    let extra_start = 8 + image_bytes as u32;
    let mut put_extra = |bytes: &[u8]| -> u32 {
        if extra.len() % 2 == 1 {
            extra.push(0);
        }
        let at = extra_start + extra.len() as u32;
        extra.extend_from_slice(bytes);
        at
    };

    let strip_offsets: Vec<u32> = (0..strips).map(|i| 8 + (i as u64 * rows_per_strip as u64 * row_bytes) as u32).collect();
    let strip_counts: Vec<u32> = (0..strips)
        .map(|i| {
            let rows = rows_per_strip.min(img.height - i * rows_per_strip);
            (rows as u64 * row_bytes) as u32
        })
        .collect();
    let le32 = |v: &[u32]| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
    let rational = |v: f64| {
        let num = (v * 100.0).round().max(1.0) as u32;
        let mut b = num.to_le_bytes().to_vec();
        b.extend_from_slice(&100u32.to_le_bytes());
        b
    };
    let software = b"Cezanne Scan\0";

    let offsets_at = if strips > 1 { put_extra(&le32(&strip_offsets)) } else { strip_offsets[0] };
    let counts_at = if strips > 1 { put_extra(&le32(&strip_counts)) } else { strip_counts[0] };
    let bps_at = if spp > 2 {
        put_extra(&(0..spp).flat_map(|_| (bps as u16).to_le_bytes()).collect::<Vec<u8>>())
    } else {
        bps
    };
    let xres_at = put_extra(&rational(img.x_dpi));
    let yres_at = put_extra(&rational(img.y_dpi));
    let soft_at = put_extra(software);
    let icc_at = icc.map(|p| put_extra(p));

    // image data
    let mut row = Vec::with_capacity(row_bytes as usize);
    let per_row = img.width as usize * img.channels;
    for y in 0..img.height as usize {
        row.clear();
        let src = &img.data[y * per_row..(y + 1) * per_row];
        if bps == 16 {
            for &v in src {
                row.extend_from_slice(&v.to_le_bytes());
            }
        } else {
            row.extend(src.iter().map(|&v| ((v as u32 * 255 + 32767) / 65535) as u8));
        }
        w.write_all(&row)?;
    }
    w.write_all(&extra)?;
    if extra.len() % 2 == 1 {
        w.write_all(&[0])?;
    }
    let ifd_at = extra_start + extra.len() as u32 + (extra.len() % 2) as u32;

    // IFD entries: (tag, type, count, value or offset)
    const SHORT: u16 = 3;
    const LONG: u16 = 4;
    const RATIONAL: u16 = 5;
    const ASCII: u16 = 2;
    const UNDEFINED: u16 = 7;
    let photometric = if spp == 3 { 2 } else { 1 };
    let mut entries: Vec<(u16, u16, u32, u32)> = vec![
        (256, LONG, 1, img.width),
        (257, LONG, 1, img.height),
        (258, SHORT, spp, bps_at),
        (259, SHORT, 1, 1),
        (262, SHORT, 1, photometric),
        (273, LONG, strips, offsets_at),
        (277, SHORT, 1, spp),
        (278, LONG, 1, rows_per_strip),
        (279, LONG, strips, counts_at),
        (282, RATIONAL, 1, xres_at),
        (283, RATIONAL, 1, yres_at),
        (284, SHORT, 1, 1),
        (296, SHORT, 1, 2),
        (305, ASCII, software.len() as u32, soft_at),
    ];
    if let (Some(at), Some(p)) = (icc_at, icc) {
        entries.push((34675, UNDEFINED, p.len() as u32, at));
    }
    entries.sort_by_key(|e| e.0);

    let mut ifd = Vec::with_capacity(2 + entries.len() * 12 + 4);
    ifd.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    for (tag, typ, count, value) in entries {
        ifd.extend_from_slice(&tag.to_le_bytes());
        ifd.extend_from_slice(&typ.to_le_bytes());
        ifd.extend_from_slice(&count.to_le_bytes());
        if typ == SHORT && count == 1 {
            ifd.extend_from_slice(&(value as u16).to_le_bytes());
            ifd.extend_from_slice(&[0, 0]);
        } else {
            ifd.extend_from_slice(&value.to_le_bytes());
        }
    }
    ifd.extend_from_slice(&0u32.to_le_bytes());
    w.write_all(&ifd)?;

    // patch the first IFD offset
    w.seek(std::io::SeekFrom::Start(4))?;
    w.write_all(&ifd_at.to_le_bytes())?;
    w.seek(std::io::SeekFrom::End(0))?;
    Ok(())
}

/// Read back the basics of a file written by [`encode`] (used by tests and the app's
/// "open previous scan" feature).
pub fn read_info(bytes: &[u8]) -> Option<(u32, u32, u16, u16)> {
    if bytes.len() < 8 || &bytes[0..4] != b"II*\0" {
        return None;
    }
    let ifd = u32::from_le_bytes(bytes[4..8].try_into().ok()?) as usize;
    let n = u16::from_le_bytes(bytes.get(ifd..ifd + 2)?.try_into().ok()?) as usize;
    let (mut w, mut h, mut bps, mut spp) = (0, 0, 0, 0);
    for i in 0..n {
        let e = ifd + 2 + i * 12;
        let tag = u16::from_le_bytes(bytes.get(e..e + 2)?.try_into().ok()?);
        let typ = u16::from_le_bytes(bytes.get(e + 2..e + 4)?.try_into().ok()?);
        let count = u32::from_le_bytes(bytes.get(e + 4..e + 8)?.try_into().ok()?);
        let v32 = u32::from_le_bytes(bytes.get(e + 8..e + 12)?.try_into().ok()?);
        let v16 = u16::from_le_bytes(bytes.get(e + 8..e + 10)?.try_into().ok()?);
        match tag {
            256 => w = v32,
            257 => h = v32,
            258 => {
                bps = if count == 1 {
                    v16
                } else {
                    u16::from_le_bytes(bytes.get(v32 as usize..v32 as usize + 2)?.try_into().ok()?)
                }
            }
            277 => spp = if typ == 3 { v16 } else { v32 as u16 },
            _ => {}
        }
    }
    Some((w, h, bps, spp))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img(channels: usize) -> Processed {
        let (w, h) = (37u32, 23u32);
        let data = (0..(w * h) as usize * channels).map(|i| (i * 97 % 65536) as u16).collect();
        Processed { width: w, height: h, channels, x_dpi: 2400.0, y_dpi: 2400.0, data }
    }

    #[test]
    fn writes_valid_16_bit_rgb_with_icc() {
        let mut c = std::io::Cursor::new(Vec::new());
        let p = super::super::icc::rgb_profile(2.2);
        encode(&mut c, &img(3), BitDepth::Sixteen, Some(&p)).unwrap();
        let b = c.into_inner();
        assert_eq!(read_info(&b), Some((37, 23, 16, 3)));
        // first pixel sample stored little endian right after the header
        assert_eq!(u16::from_le_bytes([b[8], b[9]]), 0);
        assert_eq!(u16::from_le_bytes([b[10], b[11]]), 97);
    }

    #[test]
    fn writes_8_bit_grey() {
        let mut c = std::io::Cursor::new(Vec::new());
        encode(&mut c, &img(1), BitDepth::Eight, None).unwrap();
        let b = c.into_inner();
        assert_eq!(read_info(&b), Some((37, 23, 8, 1)));
        assert_eq!(b.len() as u32 >= 8 + 37 * 23, true);
    }
}
