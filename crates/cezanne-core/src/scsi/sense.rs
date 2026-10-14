use serde::{Deserialize, Serialize};
use std::fmt;

/// SCSI sense keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SenseKey {
    NoSense,
    RecoveredError,
    NotReady,
    MediumError,
    HardwareError,
    IllegalRequest,
    UnitAttention,
    DataProtect,
    AbortedCommand,
    Other(u8),
}

impl From<u8> for SenseKey {
    fn from(v: u8) -> Self {
        match v & 0x0F {
            0x0 => SenseKey::NoSense,
            0x1 => SenseKey::RecoveredError,
            0x2 => SenseKey::NotReady,
            0x3 => SenseKey::MediumError,
            0x4 => SenseKey::HardwareError,
            0x5 => SenseKey::IllegalRequest,
            0x6 => SenseKey::UnitAttention,
            0x7 => SenseKey::DataProtect,
            0xB => SenseKey::AbortedCommand,
            other => SenseKey::Other(other),
        }
    }
}

impl From<SenseKey> for u8 {
    fn from(k: SenseKey) -> u8 {
        match k {
            SenseKey::NoSense => 0x0,
            SenseKey::RecoveredError => 0x1,
            SenseKey::NotReady => 0x2,
            SenseKey::MediumError => 0x3,
            SenseKey::HardwareError => 0x4,
            SenseKey::IllegalRequest => 0x5,
            SenseKey::UnitAttention => 0x6,
            SenseKey::DataProtect => 0x7,
            SenseKey::AbortedCommand => 0xB,
            SenseKey::Other(v) => v & 0x0F,
        }
    }
}

/// Fixed format sense data (response code 0x70 or 0x71), as built by the FT-S5000 firmware.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sense {
    pub deferred: bool,
    pub key: SenseKey,
    pub asc: u8,
    pub ascq: u8,
    /// Sense key specific bytes 15 to 17.
    pub specific: [u8; 3],
    pub raw: Vec<u8>,
}

impl Sense {
    pub const LEN: usize = 18;

    pub fn new(key: SenseKey, asc: u8, ascq: u8) -> Self {
        let mut s = Sense { deferred: false, key, asc, ascq, specific: [0; 3], raw: Vec::new() };
        s.raw = s.to_bytes().to_vec();
        s
    }

    pub fn parse(b: &[u8]) -> Option<Sense> {
        if b.len() < 3 {
            return None;
        }
        let code = b[0] & 0x7F;
        if code != 0x70 && code != 0x71 {
            return None;
        }
        let get = |i: usize| b.get(i).copied().unwrap_or(0);
        Some(Sense {
            deferred: code == 0x71,
            key: SenseKey::from(b[2]),
            asc: get(12),
            ascq: get(13),
            specific: [get(15), get(16), get(17)],
            raw: b.to_vec(),
        })
    }

    pub fn to_bytes(&self) -> [u8; Self::LEN] {
        let mut b = [0u8; Self::LEN];
        b[0] = if self.deferred { 0x71 } else { 0x70 };
        b[2] = u8::from(self.key);
        b[7] = 0x0B;
        b[12] = self.asc;
        b[13] = self.ascq;
        b[15..18].copy_from_slice(&self.specific);
        b
    }

    /// Plain language meaning, using the codes seen in the FT-S5000 firmware.
    pub fn meaning(&self) -> &'static str {
        match (self.key, self.asc, self.ascq) {
            (SenseKey::NoSense, _, _) => "no error",
            (SenseKey::NotReady, 0x04, _) => "scanner is warming up or busy",
            (SenseKey::NotReady, _, _) => "scanner not ready",
            (SenseKey::IllegalRequest, 0x20, _) => "command not supported by this scanner",
            (SenseKey::IllegalRequest, 0x24, _) => "invalid field in the command",
            (SenseKey::IllegalRequest, 0x26, _) => "invalid parameter (window, table or data type)",
            (SenseKey::IllegalRequest, 0x2C, _) => "command sequence error",
            (SenseKey::HardwareError, 0x84, _) => "EEPROM access failed",
            (SenseKey::HardwareError, 0x88, _) => "scan engine error",
            (SenseKey::HardwareError, _, _) => "hardware error",
            (SenseKey::UnitAttention, _, _) => "scanner was reset or its state changed",
            (SenseKey::AbortedCommand, _, _) => "command aborted",
            _ => "unknown condition",
        }
    }
}

impl fmt::Display for Sense {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:?} ASC 0x{:02X} ASCQ 0x{:02X} ({})",
            self.key,
            self.asc,
            self.ascq,
            self.meaning()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let s = Sense::new(SenseKey::IllegalRequest, 0x26, 0x02);
        let p = Sense::parse(&s.to_bytes()).unwrap();
        assert_eq!(p.key, SenseKey::IllegalRequest);
        assert_eq!((p.asc, p.ascq), (0x26, 0x02));
    }
}
