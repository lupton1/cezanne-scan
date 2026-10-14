//! Gamma tables for SEND(10) data type 0x03.
//!
//! A slot (qualifier 1 to 6) holds three channels of 510 bytes, which is 255 big endian
//! words per channel. The firmware stamps the channel number into the high byte of the
//! first word of each channel after download, which means entries are 8 bit output values.
//! These tables only matter for the 8 and 24 bit modes. Cezanne Scan normally scans in
//! 16 bit and does all tone work on the computer, so it leaves the scanner tables alone
//! unless asked.

use crate::error::{Error, Result};

pub const ENTRIES: usize = 255;
pub const CHANNEL_BYTES: usize = ENTRIES * 2;
pub const SLOT_BYTES: usize = CHANNEL_BYTES * 3;
pub const SLOTS: std::ops::RangeInclusive<u8> = 1..=6;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GammaTable {
    /// Red, green, blue. Each entry is an output level 0 to 255.
    pub channels: [[u8; ENTRIES]; 3],
}

impl GammaTable {
    pub fn identity() -> Self {
        let mut c = [0u8; ENTRIES];
        for (i, v) in c.iter_mut().enumerate() {
            *v = ((i * 255) / (ENTRIES - 1)) as u8;
        }
        GammaTable { channels: [c, c, c] }
    }

    /// A power law curve, `gamma` above 1 brightens mid tones.
    pub fn power(gamma: f64) -> Self {
        let mut c = [0u8; ENTRIES];
        for (i, v) in c.iter_mut().enumerate() {
            let x = i as f64 / (ENTRIES - 1) as f64;
            *v = (x.powf(1.0 / gamma) * 255.0).round().clamp(0.0, 255.0) as u8;
        }
        GammaTable { channels: [c, c, c] }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(SLOT_BYTES);
        for ch in &self.channels {
            for &e in ch {
                v.extend_from_slice(&u16::from(e).to_be_bytes());
            }
        }
        v
    }

    pub fn from_bytes(b: &[u8]) -> Result<Self> {
        if b.len() < SLOT_BYTES {
            return Err(Error::Protocol(format!("gamma slot needs {SLOT_BYTES} bytes, got {}", b.len())));
        }
        let mut t = GammaTable::identity();
        for (c, ch) in t.channels.iter_mut().enumerate() {
            for (i, e) in ch.iter_mut().enumerate() {
                let at = c * CHANNEL_BYTES + i * 2;
                // low byte is the level, the high byte of entry 0 carries the channel tag
                *e = b[at + 1];
            }
        }
        Ok(t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_round_trip() {
        let t = GammaTable::identity();
        let b = t.to_bytes();
        assert_eq!(b.len(), 1530);
        assert_eq!(GammaTable::from_bytes(&b).unwrap(), t);
        assert_eq!(t.channels[0][0], 0);
        assert_eq!(t.channels[0][ENTRIES - 1], 255);
    }
}
