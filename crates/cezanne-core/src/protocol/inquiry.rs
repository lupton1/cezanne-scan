use serde::{Deserialize, Serialize};

/// Standard INQUIRY data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Inquiry {
    pub device_type: u8,
    pub ansi_version: u8,
    pub vendor: String,
    pub product: String,
    pub revision: String,
    /// Vendor specific bytes after the revision (36 onwards).
    pub vendor_specific: Vec<u8>,
}

impl Inquiry {
    pub const LEN: usize = 36;

    pub fn parse(b: &[u8]) -> Option<Inquiry> {
        if b.len() < 36 {
            return None;
        }
        let text = |r: std::ops::Range<usize>| String::from_utf8_lossy(&b[r]).trim_end().to_string();
        Some(Inquiry {
            device_type: b[0] & 0x1F,
            ansi_version: b[2] & 0x07,
            vendor: text(8..16),
            product: text(16..32),
            revision: text(32..36),
            vendor_specific: b[36..].to_vec(),
        })
    }

    /// Build inquiry data the way the FT-S5000 firmware does (used by the simulator).
    pub fn ft_s5000_bytes(revision: &str) -> Vec<u8> {
        let mut b = vec![0u8; 36];
        b[0] = 0x06; // scanner
        b[2] = 0x02; // SCSI-2
        b[3] = 0x02; // response format
        b[4] = 0x20; // additional length
        b[7] = 0x10; // sync transfer
        b[8..16].copy_from_slice(b"SCREEN  ");
        b[16..32].copy_from_slice(b"FT-S5000        ");
        let mut rev = [b' '; 4];
        for (d, s) in rev.iter_mut().zip(revision.bytes()) {
            *d = s;
        }
        b[32..36].copy_from_slice(&rev);
        b
    }

    pub fn is_scanner(&self) -> bool {
        self.device_type == 6
    }

    pub fn is_supported(&self) -> bool {
        crate::scsi::is_supported(&self.vendor, &self.product)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_firmware_template() {
        let i = Inquiry::parse(&Inquiry::ft_s5000_bytes("2.12")).unwrap();
        assert_eq!(i.vendor, "SCREEN");
        assert_eq!(i.product, "FT-S5000");
        assert_eq!(i.revision, "2.12");
        assert!(i.is_scanner() && i.is_supported());
    }
}
