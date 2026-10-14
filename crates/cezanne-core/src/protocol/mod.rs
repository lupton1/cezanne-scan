//! FT-S5000 command set and data structures. See `docs/protocol.md` for the source of
//! every value in this module.

pub mod cdb;
pub mod gamma;
pub mod inquiry;
pub mod window;

pub use gamma::GammaTable;
pub use inquiry::Inquiry;
pub use window::{AreaUnit, ColorMode, Geometry, LightSource, ScanArea, WindowDescriptor, WindowSettings};

/// Read a big endian 24 bit length field, as used in CDB bytes 6 to 8.
pub fn be24(b: &[u8]) -> u32 {
    (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2])
}

pub fn put_be24(b: &mut [u8], v: u32) {
    b[0] = (v >> 16) as u8;
    b[1] = (v >> 8) as u8;
    b[2] = v as u8;
}

pub fn be16(b: &[u8]) -> u16 {
    u16::from_be_bytes([b[0], b[1]])
}

pub fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}
