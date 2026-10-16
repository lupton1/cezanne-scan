//! Raw SCSI transport layer.
//!
//! A [`Transport`] executes one CDB with an optional data phase and reports the status
//! byte and sense data. Everything above this layer is platform independent.

mod sense;
pub mod sim;
pub mod tcp;

#[cfg(target_os = "linux")]
pub mod linux_sg;
#[cfg(target_os = "macos")]
pub mod macos_iokit;
#[cfg(windows)]
pub mod windows_spti;

pub use sense::{Sense, SenseKey};

use crate::error::{Error, Result};
use std::time::Duration;

/// SCSI status byte values used by the scanner.
pub mod status {
    pub const GOOD: u8 = 0x00;
    pub const CHECK_CONDITION: u8 = 0x02;
    pub const BUSY: u8 = 0x08;
    pub const RESERVATION_CONFLICT: u8 = 0x18;
}

/// The data phase of a command.
pub enum DataPhase<'a> {
    None,
    /// Device to host. The buffer length is the allocation length.
    In(&'a mut [u8]),
    /// Host to device.
    Out(&'a [u8]),
}

impl DataPhase<'_> {
    pub fn len(&self) -> usize {
        match self {
            DataPhase::None => 0,
            DataPhase::In(b) => b.len(),
            DataPhase::Out(b) => b.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Result of one executed command.
#[derive(Debug, Clone)]
pub struct Completion {
    pub status: u8,
    /// Bytes actually moved in the data phase.
    pub transferred: usize,
    /// Sense data when the status was CHECK CONDITION and the transport fetched it
    /// automatically (SG_IO, SPTI and the bridge all do).
    pub sense: Option<Sense>,
}

impl Completion {
    pub fn good(transferred: usize) -> Self {
        Completion { status: status::GOOD, transferred, sense: None }
    }

    pub fn check(sense: Sense) -> Self {
        Completion { status: status::CHECK_CONDITION, transferred: 0, sense: Some(sense) }
    }
}

/// Something that can run SCSI commands against one target device.
pub trait Transport: Send {
    fn execute(&mut self, cdb: &[u8], data: DataPhase<'_>, timeout: Duration) -> Result<Completion>;

    /// Human readable description, for example `SG_IO /dev/sg3` or `bridge 10.0.0.5:7810`.
    fn describe(&self) -> String;
}

impl<T: Transport + ?Sized> Transport for Box<T> {
    fn execute(&mut self, cdb: &[u8], data: DataPhase<'_>, timeout: Duration) -> Result<Completion> {
        (**self).execute(cdb, data, timeout)
    }

    fn describe(&self) -> String {
        (**self).describe()
    }
}

/// Run a command and turn a non GOOD status into an [`Error::Scsi`].
pub fn run(t: &mut dyn Transport, cdb: &[u8], data: DataPhase<'_>, timeout: Duration) -> Result<usize> {
    check_allowed(cdb)?;
    let c = t.execute(cdb, data, timeout)?;
    if c.status == status::GOOD {
        Ok(c.transferred)
    } else {
        Err(Error::Scsi { opcode: cdb[0], status: c.status, sense: c.sense })
    }
}

/// Commands this project never sends, whatever the caller asks for.
///
/// They write firmware or factory data into the scanner and a mistake could leave it
/// unusable. The check runs in the driver, in the bridge and in the simulator.
pub fn check_allowed(cdb: &[u8]) -> Result<()> {
    let Some(&op) = cdb.first() else {
        return Err(Error::InvalidParameter("empty CDB".into()));
    };
    match op {
        // WRITE BUFFER: firmware download.
        0x3B => Err(Error::Blocked("WRITE BUFFER (firmware download)".into())),
        // Vendor maintenance write: mechanics parameters, positions, raw JOANA commands.
        0xC9 => Err(Error::Blocked("vendor maintenance write (0xC9)".into())),
        // SEND(10) with data types that end up in EEPROM, or with the save bit set.
        0x2A => {
            let dtc = cdb.get(2).copied().unwrap_or(0);
            let save = cdb.get(4).copied().unwrap_or(0) & 1 != 0;
            match dtc {
                0x8A => Err(Error::Blocked("SEND memory switches (0x8A)".into())),
                0x8B => Err(Error::Blocked("SEND factory ROM data (0x8B)".into())),
                0x8C => Err(Error::Blocked("SEND serial number (0x8C)".into())),
                _ if save => Err(Error::Blocked("SEND with the save to EEPROM bit".into())),
                _ => Ok(()),
            }
        }
        _ => Ok(()),
    }
}

/// A scanner found during discovery.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DeviceInfo {
    /// Platform specific address, for example `/dev/sg3` or `scsi2:0:5:0`.
    pub address: String,
    pub vendor: String,
    pub product: String,
    pub revision: String,
}

/// Find FT-S5000 class scanners attached to this machine.
pub fn discover() -> Result<Vec<DeviceInfo>> {
    #[cfg(target_os = "linux")]
    {
        linux_sg::discover()
    }
    #[cfg(windows)]
    {
        windows_spti::discover()
    }
    #[cfg(target_os = "macos")]
    {
        macos_iokit::discover()
    }
    #[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
    {
        Err(Error::Unsupported(
            "this OS has no direct SCSI access, connect through cezanne-bridge".into(),
        ))
    }
}

/// Open a local device by the address returned from [`discover`].
pub fn open_local(address: &str) -> Result<Box<dyn Transport>> {
    #[cfg(target_os = "linux")]
    {
        Ok(Box::new(linux_sg::SgTransport::open(address)?))
    }
    #[cfg(windows)]
    {
        Ok(Box::new(windows_spti::SptiTransport::open(address)?))
    }
    #[cfg(target_os = "macos")]
    {
        Ok(Box::new(macos_iokit::IoKitTransport::open(address)?))
    }
    #[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
    {
        let _ = address;
        Err(Error::Unsupported(
            "this OS has no direct SCSI access, connect through cezanne-bridge".into(),
        ))
    }
}

/// True when the inquiry strings belong to a scanner this driver supports.
pub fn is_supported(vendor: &str, product: &str) -> bool {
    vendor.trim() == "SCREEN" && (product.starts_with("FT-S5") || product.starts_with("FT-S3"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dangerous_commands_are_blocked() {
        assert!(check_allowed(&[0x3B, 0, 0, 0, 0, 0, 0, 0, 0, 0]).is_err());
        assert!(check_allowed(&[0xC9, 0, 0, 0, 0, 0, 0, 0, 0, 0]).is_err());
        for dtc in [0x8A, 0x8B, 0x8C] {
            assert!(check_allowed(&[0x2A, 0, dtc, 0, 0, 1, 0, 0, 16, 0]).is_err());
        }
        // gamma with the save bit
        assert!(check_allowed(&[0x2A, 0, 0x03, 0, 1, 1, 0, 5, 0xFA, 0]).is_err());
    }

    #[test]
    fn normal_commands_pass() {
        assert!(check_allowed(&[0x00, 0, 0, 0, 0, 0]).is_ok());
        assert!(check_allowed(&[0x12, 0, 0, 0, 36, 0]).is_ok());
        assert!(check_allowed(&[0x2A, 0, 0x03, 0, 0, 1, 0, 5, 0xFA, 0]).is_ok());
    }
}
