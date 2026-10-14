//! Linux SCSI generic (`/dev/sg*`) transport using the SG_IO ioctl.
//!
//! The user needs read and write access to the sg node. The packaged udev rule
//! (`packaging/linux/60-cezanne-scan.rules`) grants it to the `scanner` group.

use super::{Completion, DataPhase, DeviceInfo, Sense, Transport};
use crate::error::{Error, Result};
use crate::protocol::{cdb, Inquiry};
use std::fs::{File, OpenOptions};
use std::os::unix::io::AsRawFd;
use std::time::Duration;

const SG_IO: libc::c_ulong = 0x2285;
const SG_DXFER_NONE: i32 = -1;
const SG_DXFER_TO_DEV: i32 = -2;
const SG_DXFER_FROM_DEV: i32 = -3;

#[repr(C)]
struct SgIoHdr {
    interface_id: i32,
    dxfer_direction: i32,
    cmd_len: u8,
    mx_sb_len: u8,
    iovec_count: u16,
    dxfer_len: u32,
    dxferp: *mut libc::c_void,
    cmdp: *const u8,
    sbp: *mut u8,
    timeout: u32,
    flags: u32,
    pack_id: i32,
    usr_ptr: *mut libc::c_void,
    status: u8,
    masked_status: u8,
    msg_status: u8,
    sb_len_wr: u8,
    host_status: u16,
    driver_status: u16,
    resid: i32,
    duration: u32,
    info: u32,
}

pub struct SgTransport {
    file: File,
    path: String,
}

impl SgTransport {
    pub fn open(path: &str) -> Result<Self> {
        let file = OpenOptions::new().read(true).write(true).open(path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::PermissionDenied {
                Error::Io(std::io::Error::new(
                    e.kind(),
                    format!("{path}: permission denied. Add yourself to the scanner group or install the udev rule"),
                ))
            } else {
                Error::Io(e)
            }
        })?;
        Ok(SgTransport { file, path: path.to_string() })
    }
}

impl Transport for SgTransport {
    fn execute(&mut self, c: &[u8], data: DataPhase<'_>, timeout: Duration) -> Result<Completion> {
        let mut sense = [0u8; 32];
        let (dir, ptr, len) = match data {
            DataPhase::None => (SG_DXFER_NONE, std::ptr::null_mut(), 0usize),
            DataPhase::In(b) => (SG_DXFER_FROM_DEV, b.as_mut_ptr() as *mut libc::c_void, b.len()),
            DataPhase::Out(b) => (SG_DXFER_TO_DEV, b.as_ptr() as *mut libc::c_void, b.len()),
        };
        let mut h = SgIoHdr {
            interface_id: b'S' as i32,
            dxfer_direction: dir,
            cmd_len: c.len() as u8,
            mx_sb_len: sense.len() as u8,
            iovec_count: 0,
            dxfer_len: len as u32,
            dxferp: ptr,
            cmdp: c.as_ptr(),
            sbp: sense.as_mut_ptr(),
            timeout: timeout.as_millis().min(u32::MAX as u128) as u32,
            flags: 0,
            pack_id: 0,
            usr_ptr: std::ptr::null_mut(),
            status: 0,
            masked_status: 0,
            msg_status: 0,
            sb_len_wr: 0,
            host_status: 0,
            driver_status: 0,
            resid: 0,
            duration: 0,
            info: 0,
        };
        // SAFETY: the header points at buffers that live for the duration of the call.
        let rc = unsafe { libc::ioctl(self.file.as_raw_fd(), SG_IO as _, &mut h as *mut SgIoHdr) };
        if rc < 0 {
            return Err(Error::Io(std::io::Error::last_os_error()));
        }
        if h.host_status != 0 {
            return Err(Error::Protocol(format!("SCSI host adapter error 0x{:X}", h.host_status)));
        }
        let transferred = len.saturating_sub(h.resid.max(0) as usize);
        let sense = if h.sb_len_wr > 0 { Sense::parse(&sense[..h.sb_len_wr as usize]) } else { None };
        Ok(Completion { status: h.status, transferred, sense })
    }

    fn describe(&self) -> String {
        format!("SG_IO {}", self.path)
    }
}

pub fn discover() -> Result<Vec<DeviceInfo>> {
    let mut found = Vec::new();
    let mut entries: Vec<_> = std::fs::read_dir("/dev")?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("sg")))
        .collect();
    entries.sort();
    for p in entries {
        let path = p.to_string_lossy().to_string();
        let Ok(mut t) = SgTransport::open(&path) else { continue };
        let mut buf = [0u8; 36];
        let ok = t
            .execute(&cdb::inquiry(false, 0, 36), DataPhase::In(&mut buf), Duration::from_secs(5))
            .map(|c| c.status == 0)
            .unwrap_or(false);
        if !ok {
            continue;
        }
        if let Some(i) = Inquiry::parse(&buf) {
            if i.is_supported() {
                found.push(DeviceInfo { address: path, vendor: i.vendor, product: i.product, revision: i.revision });
            }
        }
    }
    Ok(found)
}
