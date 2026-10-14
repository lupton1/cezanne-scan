//! Windows SCSI Pass Through Interface transport.
//!
//! Scanners are not claimed by a Windows class driver, so commands go to the adapter
//! (`\\.\ScsiN:`) with an explicit path, target and LUN. This needs an elevated process on
//! most systems. Addresses look like `scsi2:0:5:0` (adapter, path, target, lun).

use super::{Completion, DataPhase, DeviceInfo, Sense, Transport};
use crate::error::{Error, Result};
use crate::protocol::Inquiry;
use std::ffi::c_void;
use std::time::Duration;
use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING};
use windows_sys::Win32::System::IO::DeviceIoControl;

const IOCTL_SCSI_PASS_THROUGH_DIRECT: u32 = 0x0004_D014;
const IOCTL_SCSI_GET_INQUIRY_DATA: u32 = 0x0004_100C;
const SCSI_IOCTL_DATA_OUT: u8 = 0;
const SCSI_IOCTL_DATA_IN: u8 = 1;
const SCSI_IOCTL_DATA_UNSPECIFIED: u8 = 2;

#[repr(C)]
struct ScsiPassThroughDirect {
    length: u16,
    scsi_status: u8,
    path_id: u8,
    target_id: u8,
    lun: u8,
    cdb_length: u8,
    sense_info_length: u8,
    data_in: u8,
    data_transfer_length: u32,
    timeout_value: u32,
    data_buffer: *mut c_void,
    sense_info_offset: u32,
    cdb: [u8; 16],
}

#[repr(C)]
struct SptdWithSense {
    sptd: ScsiPassThroughDirect,
    filler: u32,
    sense: [u8; 32],
}

/// Address of one device: adapter number plus SCSI path, target and LUN.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SptiAddress {
    pub adapter: u32,
    pub path: u8,
    pub target: u8,
    pub lun: u8,
}

impl SptiAddress {
    pub fn parse(s: &str) -> Result<Self> {
        let bad = || Error::InvalidParameter(format!("expected scsiA:P:T:L, got {s}"));
        let rest = s.strip_prefix("scsi").ok_or_else(bad)?;
        let parts: Vec<&str> = rest.split(':').collect();
        if parts.len() != 4 {
            return Err(bad());
        }
        Ok(SptiAddress {
            adapter: parts[0].parse().map_err(|_| bad())?,
            path: parts[1].parse().map_err(|_| bad())?,
            target: parts[2].parse().map_err(|_| bad())?,
            lun: parts[3].parse().map_err(|_| bad())?,
        })
    }
}

impl std::fmt::Display for SptiAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "scsi{}:{}:{}:{}", self.adapter, self.path, self.target, self.lun)
    }
}

struct Handle(HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the handle came from CreateFileW and is closed once.
        unsafe { CloseHandle(self.0) };
    }
}

// SAFETY: a device handle can be used from any thread.
unsafe impl Send for Handle {}

fn open_adapter(n: u32) -> Result<Handle> {
    let name: Vec<u16> = format!("\\\\.\\Scsi{n}:").encode_utf16().chain(Some(0)).collect();
    // SAFETY: name is a NUL terminated wide string.
    let h = unsafe {
        CreateFileW(
            name.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        )
    };
    if h == INVALID_HANDLE_VALUE {
        // SAFETY: plain FFI call.
        let code = unsafe { GetLastError() };
        let hint = if code == 5 { " (access denied: run Cezanne Scan as administrator)" } else { "" };
        return Err(Error::Io(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("cannot open SCSI adapter {n}: error {code}{hint}"),
        )));
    }
    Ok(Handle(h))
}

pub struct SptiTransport {
    handle: Handle,
    addr: SptiAddress,
}

impl SptiTransport {
    pub fn open(address: &str) -> Result<Self> {
        let addr = SptiAddress::parse(address)?;
        Ok(SptiTransport { handle: open_adapter(addr.adapter)?, addr })
    }
}

impl Transport for SptiTransport {
    fn execute(&mut self, c: &[u8], data: DataPhase<'_>, timeout: Duration) -> Result<Completion> {
        if c.len() > 16 {
            return Err(Error::InvalidParameter("CDB longer than 16 bytes".into()));
        }
        let (dir, ptr, len) = match data {
            DataPhase::None => (SCSI_IOCTL_DATA_UNSPECIFIED, std::ptr::null_mut(), 0usize),
            DataPhase::In(b) => (SCSI_IOCTL_DATA_IN, b.as_mut_ptr() as *mut c_void, b.len()),
            DataPhase::Out(b) => (SCSI_IOCTL_DATA_OUT, b.as_ptr() as *mut c_void, b.len()),
        };
        let mut cdb = [0u8; 16];
        cdb[..c.len()].copy_from_slice(c);
        let mut s = SptdWithSense {
            sptd: ScsiPassThroughDirect {
                length: std::mem::size_of::<ScsiPassThroughDirect>() as u16,
                scsi_status: 0,
                path_id: self.addr.path,
                target_id: self.addr.target,
                lun: self.addr.lun,
                cdb_length: c.len() as u8,
                sense_info_length: 32,
                data_in: dir,
                data_transfer_length: len as u32,
                timeout_value: timeout.as_secs().clamp(1, 108_000) as u32,
                data_buffer: ptr,
                sense_info_offset: std::mem::offset_of!(SptdWithSense, sense) as u32,
                cdb,
            },
            filler: 0,
            sense: [0; 32],
        };
        let size = std::mem::size_of::<SptdWithSense>() as u32;
        let mut returned = 0u32;
        // SAFETY: the structure and data buffer stay alive for this synchronous call.
        let ok = unsafe {
            DeviceIoControl(
                self.handle.0,
                IOCTL_SCSI_PASS_THROUGH_DIRECT,
                &mut s as *mut _ as *mut c_void,
                size,
                &mut s as *mut _ as *mut c_void,
                size,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            // SAFETY: plain FFI call.
            let code = unsafe { GetLastError() };
            return Err(Error::Io(std::io::Error::new(std::io::ErrorKind::Other, format!("SPTI error {code}"))));
        }
        let sense = if s.sptd.scsi_status != 0 { Sense::parse(&s.sense) } else { None };
        Ok(Completion { status: s.sptd.scsi_status, transferred: s.sptd.data_transfer_length as usize, sense })
    }

    fn describe(&self) -> String {
        format!("SPTI {}", self.addr)
    }
}

/// Walk adapters 0 to 15 and report every device whose inquiry data names a supported
/// SCREEN scanner. Uses IOCTL_SCSI_GET_INQUIRY_DATA, so no commands go to the devices.
pub fn discover() -> Result<Vec<DeviceInfo>> {
    let mut found = Vec::new();
    let mut denied = false;
    for n in 0..16u32 {
        let h = match open_adapter(n) {
            Ok(h) => h,
            Err(e) => {
                denied |= e.to_string().contains("access denied");
                continue;
            }
        };
        let mut buf = vec![0u8; 16 * 1024];
        let mut returned = 0u32;
        // SAFETY: output buffer is valid for its length.
        let ok = unsafe {
            DeviceIoControl(
                h.0,
                IOCTL_SCSI_GET_INQUIRY_DATA,
                std::ptr::null(),
                0,
                buf.as_mut_ptr() as *mut c_void,
                buf.len() as u32,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            continue;
        }
        // SCSI_ADAPTER_BUS_INFO: u8 NumberOfBuses, then from offset 4 one 8 byte
        // SCSI_BUS_DATA per bus {u8 LogicalUnits, u8 InitiatorBusId, pad, u32 InquiryDataOffset}
        let buses = buf[0] as usize;
        for b in 0..buses {
            let at = 4 + b * 8;
            if at + 8 > buf.len() {
                break;
            }
            let mut off = u32::from_le_bytes([buf[at + 4], buf[at + 5], buf[at + 6], buf[at + 7]]) as usize;
            // SCSI_INQUIRY_DATA: PathId, TargetId, Lun, DeviceClaimed, u32 InquiryDataLength,
            // u32 NextInquiryDataOffset, InquiryData[]
            while off != 0 && off + 12 < buf.len() {
                let path = buf[off];
                let target = buf[off + 1];
                let lun = buf[off + 2];
                let len = u32::from_le_bytes([buf[off + 4], buf[off + 5], buf[off + 6], buf[off + 7]]) as usize;
                let next = u32::from_le_bytes([buf[off + 8], buf[off + 9], buf[off + 10], buf[off + 11]]) as usize;
                let data = &buf[off + 12..(off + 12 + len).min(buf.len())];
                if let Some(i) = Inquiry::parse(data) {
                    if i.is_supported() {
                        let addr = SptiAddress { adapter: n, path, target, lun };
                        found.push(DeviceInfo {
                            address: addr.to_string(),
                            vendor: i.vendor,
                            product: i.product,
                            revision: i.revision,
                        });
                    }
                }
                off = next;
            }
        }
    }
    if found.is_empty() && denied {
        return Err(Error::Io(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "the SCSI adapters could not be opened: run Cezanne Scan as administrator",
        )));
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_round_trip() {
        let a = SptiAddress::parse("scsi2:0:5:0").unwrap();
        assert_eq!(a, SptiAddress { adapter: 2, path: 0, target: 5, lun: 0 });
        assert_eq!(a.to_string(), "scsi2:0:5:0");
        assert!(SptiAddress::parse("2:0:5").is_err());
    }
}
