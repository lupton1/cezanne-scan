//! macOS transport through IOKit's SCSITaskDeviceInterface (SCSITaskLib).
//!
//! macOS has no kernel driver for SCSI scanners, so the SCSI Architecture Model family
//! publishes them as "SCSITaskUserClientDevice" nubs that an application can open with
//! exclusive access and send raw CDBs to. This is the same route SANE uses on the Mac.
//!
//! It needs a SCSI host adapter with a macOS driver (for example an ATTO card in a
//! Thunderbolt PCIe enclosure on an Intel Mac). Addresses look like `iokit:<registry id>`.
//!
//! Status: written against Apple's SCSITaskLib interface but not yet run on hardware.

use super::{Completion, DataPhase, DeviceInfo, Sense, Transport};
use crate::error::{Error, Result};
use std::ffi::{c_char, c_void, CStr, CString};
use std::time::Duration;

type CFTypeRef = *const c_void;
type CFStringRef = *const c_void;
type CFDictionaryRef = *const c_void;
type CFMutableDictionaryRef = *mut c_void;
type CFUUIDRef = *const c_void;
type CFAllocatorRef = *const c_void;
type IoObject = u32;
type KernReturn = i32;
type IoReturn = i32;

const KCF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
const KCF_NUMBER_SINT32_TYPE: i32 = 3;
const KIO_MAIN_PORT_DEFAULT: u32 = 0;

const DIR_NONE: u8 = 0;
const DIR_TO_TARGET: u8 = 1;
const DIR_FROM_TARGET: u8 = 2;

#[repr(C)]
struct Opaque {
    _p: [u8; 0],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CFUUIDBytes {
    b: [u8; 16],
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFAllocatorDefault: CFAllocatorRef;
    static kCFTypeDictionaryKeyCallBacks: Opaque;
    static kCFTypeDictionaryValueCallBacks: Opaque;
    fn CFDictionaryCreateMutable(a: CFAllocatorRef, cap: isize, k: *const Opaque, v: *const Opaque) -> CFMutableDictionaryRef;
    fn CFDictionarySetValue(d: CFMutableDictionaryRef, key: *const c_void, value: *const c_void);
    fn CFStringCreateWithCString(a: CFAllocatorRef, s: *const c_char, enc: u32) -> CFStringRef;
    fn CFStringGetCString(s: CFStringRef, buf: *mut c_char, len: isize, enc: u32) -> u8;
    fn CFGetTypeID(t: CFTypeRef) -> usize;
    fn CFStringGetTypeID() -> usize;
    fn CFNumberGetTypeID() -> usize;
    fn CFNumberGetValue(n: CFTypeRef, kind: i32, out: *mut c_void) -> u8;
    fn CFRelease(t: CFTypeRef);
    fn CFUUIDGetConstantUUIDWithBytes(a: CFAllocatorRef, b0: u8, b1: u8, b2: u8, b3: u8, b4: u8, b5: u8, b6: u8, b7: u8, b8: u8, b9: u8, b10: u8, b11: u8, b12: u8, b13: u8, b14: u8, b15: u8) -> CFUUIDRef;
    fn CFUUIDGetUUIDBytes(u: CFUUIDRef) -> CFUUIDBytes;
}

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    fn IOServiceGetMatchingServices(port: u32, matching: CFDictionaryRef, iter: *mut IoObject) -> KernReturn;
    fn IOServiceGetMatchingService(port: u32, matching: CFDictionaryRef) -> IoObject;
    fn IOIteratorNext(iter: IoObject) -> IoObject;
    fn IOObjectRelease(o: IoObject) -> KernReturn;
    fn IORegistryEntryCreateCFProperty(e: IoObject, key: CFStringRef, a: CFAllocatorRef, opts: u32) -> CFTypeRef;
    fn IORegistryEntryGetRegistryEntryID(e: IoObject, id: *mut u64) -> KernReturn;
    fn IORegistryEntryIDMatching(id: u64) -> CFMutableDictionaryRef;
    fn IOCreatePlugInInterfaceForService(
        service: IoObject,
        plugin_type: CFUUIDRef,
        interface_type: CFUUIDRef,
        the_interface: *mut *mut *mut IOCFPlugInInterface,
        score: *mut i32,
    ) -> KernReturn;
}

// COM style interfaces from IOCFPlugIn.h and SCSITaskLib.h. Field order matters.

#[repr(C)]
struct IOCFPlugInInterface {
    _reserved: *mut c_void,
    query_interface: unsafe extern "C" fn(*mut c_void, CFUUIDBytes, *mut *mut c_void) -> i32,
    add_ref: unsafe extern "C" fn(*mut c_void) -> u32,
    release: unsafe extern "C" fn(*mut c_void) -> u32,
    version: u16,
    revision: u16,
    probe: *const c_void,
    start: *const c_void,
    stop: *const c_void,
}

#[repr(C)]
struct SCSITaskDeviceInterface {
    _reserved: *mut c_void,
    query_interface: unsafe extern "C" fn(*mut c_void, CFUUIDBytes, *mut *mut c_void) -> i32,
    add_ref: unsafe extern "C" fn(*mut c_void) -> u32,
    release: unsafe extern "C" fn(*mut c_void) -> u32,
    version: u16,
    revision: u16,
    is_exclusive_access_available: unsafe extern "C" fn(*mut c_void) -> u8,
    add_callback_dispatcher_to_run_loop: *const c_void,
    remove_callback_dispatcher_from_run_loop: *const c_void,
    obtain_exclusive_access: unsafe extern "C" fn(*mut c_void) -> IoReturn,
    release_exclusive_access: unsafe extern "C" fn(*mut c_void) -> IoReturn,
    create_scsi_task: unsafe extern "C" fn(*mut c_void) -> *mut *mut SCSITaskInterface,
}

#[repr(C)]
struct IOVirtualRange {
    address: usize,
    length: u64,
}

#[repr(C)]
struct SCSITaskInterface {
    _reserved: *mut c_void,
    query_interface: unsafe extern "C" fn(*mut c_void, CFUUIDBytes, *mut *mut c_void) -> i32,
    add_ref: unsafe extern "C" fn(*mut c_void) -> u32,
    release: unsafe extern "C" fn(*mut c_void) -> u32,
    version: u16,
    revision: u16,
    is_task_active: *const c_void,
    set_task_attribute: *const c_void,
    get_task_attribute: *const c_void,
    set_command_descriptor_block: unsafe extern "C" fn(*mut c_void, *const u8, u8) -> IoReturn,
    get_command_descriptor_block_size: *const c_void,
    get_command_descriptor_block: *const c_void,
    set_scatter_gather_entries: unsafe extern "C" fn(*mut c_void, *const IOVirtualRange, u8, u64, u8) -> IoReturn,
    set_timeout_duration: unsafe extern "C" fn(*mut c_void, u32) -> IoReturn,
    get_timeout_duration: *const c_void,
    set_task_completion_callback: *const c_void,
    execute_task_async: *const c_void,
    execute_task_sync: unsafe extern "C" fn(*mut c_void, *mut u8, *mut u32, *mut u64) -> IoReturn,
}

fn uuid(b: [u8; 16]) -> CFUUIDRef {
    // SAFETY: plain FFI call, constant UUIDs are owned by CoreFoundation.
    unsafe {
        CFUUIDGetConstantUUIDWithBytes(
            std::ptr::null(),
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15],
        )
    }
}

/// kIOSCSITaskDeviceUserClientTypeID
const USER_CLIENT_TYPE: [u8; 16] = [0x7D, 0x66, 0x67, 0x8E, 0x08, 0xA2, 0x11, 0xD5, 0xA1, 0xB8, 0x00, 0x30, 0x65, 0x7D, 0x05, 0x2A];
/// kIOSCSITaskDeviceInterfaceID
const DEVICE_INTERFACE: [u8; 16] = [0x1B, 0xBC, 0x41, 0x32, 0x08, 0xA5, 0x11, 0xD5, 0x90, 0xED, 0x00, 0x30, 0x65, 0x7D, 0x05, 0x2A];
/// kIOCFPlugInInterfaceID
const PLUGIN_INTERFACE: [u8; 16] = [0xC2, 0x44, 0xE8, 0x58, 0x10, 0x9C, 0x11, 0xD4, 0x91, 0xD4, 0x00, 0x50, 0xE4, 0xC6, 0x42, 0x6F];

struct CfString(CFStringRef);

impl CfString {
    fn new(s: &str) -> CfString {
        let c = CString::new(s).expect("no NUL in key");
        // SAFETY: c is a valid C string for the duration of the call.
        CfString(unsafe { CFStringCreateWithCString(kCFAllocatorDefault, c.as_ptr(), KCF_STRING_ENCODING_UTF8) })
    }
}

impl Drop for CfString {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: created by CFStringCreateWithCString.
            unsafe { CFRelease(self.0) };
        }
    }
}

fn string_property(entry: IoObject, key: &str) -> Option<String> {
    let k = CfString::new(key);
    // SAFETY: entry is a live registry entry, k a valid CFString.
    unsafe {
        let v = IORegistryEntryCreateCFProperty(entry, k.0, kCFAllocatorDefault, 0);
        if v.is_null() {
            return None;
        }
        let mut out = None;
        if CFGetTypeID(v) == CFStringGetTypeID() {
            let mut buf = [0 as c_char; 256];
            if CFStringGetCString(v, buf.as_mut_ptr(), buf.len() as isize, KCF_STRING_ENCODING_UTF8) != 0 {
                out = Some(CStr::from_ptr(buf.as_ptr()).to_string_lossy().trim().to_string());
            }
        }
        CFRelease(v);
        out
    }
}

fn number_property(entry: IoObject, key: &str) -> Option<i32> {
    let k = CfString::new(key);
    // SAFETY: as above.
    unsafe {
        let v = IORegistryEntryCreateCFProperty(entry, k.0, kCFAllocatorDefault, 0);
        if v.is_null() {
            return None;
        }
        let mut n = 0i32;
        let ok = CFGetTypeID(v) == CFNumberGetTypeID()
            && CFNumberGetValue(v, KCF_NUMBER_SINT32_TYPE, &mut n as *mut i32 as *mut c_void) != 0;
        CFRelease(v);
        ok.then_some(n)
    }
}

/// Matching dictionary for all SCSI devices that user space may drive.
fn user_client_matching() -> CFMutableDictionaryRef {
    // SAFETY: standard CoreFoundation dictionary construction.
    unsafe {
        let outer = CFDictionaryCreateMutable(kCFAllocatorDefault, 0, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
        let inner = CFDictionaryCreateMutable(kCFAllocatorDefault, 0, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
        let category = CfString::new("SCSITaskDeviceCategory");
        let user_client = CfString::new("SCSITaskUserClientDevice");
        let property_match = CfString::new("IOPropertyMatch");
        CFDictionarySetValue(inner, category.0, user_client.0);
        CFDictionarySetValue(outer, property_match.0, inner as *const c_void);
        CFRelease(inner as CFTypeRef);
        outer
    }
}

pub fn discover() -> Result<Vec<DeviceInfo>> {
    let mut found = Vec::new();
    let mut iter: IoObject = 0;
    // SAFETY: the matching dictionary is consumed by IOServiceGetMatchingServices.
    let kr = unsafe { IOServiceGetMatchingServices(KIO_MAIN_PORT_DEFAULT, user_client_matching(), &mut iter) };
    if kr != 0 {
        return Err(Error::Io(std::io::Error::other(format!("IOKit device search failed (0x{kr:x})"))));
    }
    loop {
        // SAFETY: iter is a valid iterator until released below.
        let service = unsafe { IOIteratorNext(iter) };
        if service == 0 {
            break;
        }
        let vendor = string_property(service, "Vendor Identification").unwrap_or_default();
        let product = string_property(service, "Product Identification").unwrap_or_default();
        let revision = string_property(service, "Product Revision Level").unwrap_or_default();
        let device_type = number_property(service, "Peripheral Device Type").unwrap_or(-1);
        let mut id = 0u64;
        // SAFETY: service is live.
        unsafe { IORegistryEntryGetRegistryEntryID(service, &mut id) };
        log::debug!("IOKit SCSI device {vendor} {product} type {device_type} id {id}");
        if super::is_supported(&vendor, &product) {
            found.push(DeviceInfo { address: format!("iokit:{id}"), vendor, product, revision });
        }
        // SAFETY: release our reference to the service.
        unsafe { IOObjectRelease(service) };
    }
    // SAFETY: release the iterator.
    unsafe { IOObjectRelease(iter) };
    Ok(found)
}

pub struct IoKitTransport {
    device: *mut *mut SCSITaskDeviceInterface,
    address: String,
}

// SAFETY: the interface is only used from the thread that currently owns the transport,
// and SCSITaskLib allows use from any thread.
unsafe impl Send for IoKitTransport {}

impl IoKitTransport {
    pub fn open(address: &str) -> Result<Self> {
        let id: u64 = address
            .strip_prefix("iokit:")
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| Error::InvalidParameter(format!("expected iokit:<registry id>, got {address}")))?;
        // SAFETY: IOKit calls with checked results, every reference is released on all paths.
        unsafe {
            let service = IOServiceGetMatchingService(KIO_MAIN_PORT_DEFAULT, IORegistryEntryIDMatching(id));
            if service == 0 {
                return Err(Error::NotFound(format!("no IOKit device with id {id}, run cezanne-cli list again")));
            }
            let mut plugin: *mut *mut IOCFPlugInInterface = std::ptr::null_mut();
            let mut score = 0i32;
            let kr = IOCreatePlugInInterfaceForService(service, uuid(USER_CLIENT_TYPE), uuid(PLUGIN_INTERFACE), &mut plugin, &mut score);
            IOObjectRelease(service);
            if kr != 0 || plugin.is_null() {
                return Err(Error::Io(std::io::Error::other(format!("cannot open the SCSI user client (0x{kr:x})"))));
            }
            let mut device: *mut c_void = std::ptr::null_mut();
            let hr = ((**plugin).query_interface)(plugin as *mut c_void, CFUUIDGetUUIDBytes(uuid(DEVICE_INTERFACE)), &mut device);
            ((**plugin).release)(plugin as *mut c_void);
            if hr != 0 || device.is_null() {
                return Err(Error::Io(std::io::Error::other(format!("SCSITaskDeviceInterface not available (0x{hr:x})"))));
            }
            let device = device as *mut *mut SCSITaskDeviceInterface;
            let ir = ((**device).obtain_exclusive_access)(device as *mut c_void);
            if ir != 0 {
                ((**device).release)(device as *mut c_void);
                return Err(Error::Io(std::io::Error::other(format!(
                    "the scanner is in use by another program (0x{ir:x}). Quit other scanning software and try again"
                ))));
            }
            Ok(IoKitTransport { device, address: address.to_string() })
        }
    }
}

impl Drop for IoKitTransport {
    fn drop(&mut self) {
        // SAFETY: device is valid until released here, once.
        unsafe {
            ((**self.device).release_exclusive_access)(self.device as *mut c_void);
            ((**self.device).release)(self.device as *mut c_void);
        }
    }
}

impl Transport for IoKitTransport {
    fn execute(&mut self, c: &[u8], data: DataPhase<'_>, timeout: Duration) -> Result<Completion> {
        if c.len() > 16 {
            return Err(Error::InvalidParameter("CDB longer than 16 bytes".into()));
        }
        let (dir, ptr, len) = match data {
            DataPhase::None => (DIR_NONE, 0usize, 0usize),
            DataPhase::In(b) => (DIR_FROM_TARGET, b.as_mut_ptr() as usize, b.len()),
            DataPhase::Out(b) => (DIR_TO_TARGET, b.as_ptr() as usize, b.len()),
        };
        // SAFETY: the task is created, used synchronously and released in this call. The
        // data buffer outlives the synchronous execution.
        unsafe {
            let task = ((**self.device).create_scsi_task)(self.device as *mut c_void);
            if task.is_null() {
                return Err(Error::Io(std::io::Error::other("cannot create a SCSI task")));
            }
            let t = task as *mut c_void;
            let result = (|| {
                let r = ((**task).set_command_descriptor_block)(t, c.as_ptr(), c.len() as u8);
                if r != 0 {
                    return Err(Error::Io(std::io::Error::other(format!("SetCommandDescriptorBlock failed (0x{r:x})"))));
                }
                let range = IOVirtualRange { address: ptr, length: len as u64 };
                let entries = if len > 0 { 1 } else { 0 };
                let r = ((**task).set_scatter_gather_entries)(t, &range, entries, len as u64, dir);
                if r != 0 {
                    return Err(Error::Io(std::io::Error::other(format!("SetScatterGatherEntries failed (0x{r:x})"))));
                }
                ((**task).set_timeout_duration)(t, timeout.as_millis().min(u32::MAX as u128) as u32);
                let mut sense = [0u8; 18];
                let mut status = 0u32;
                let mut transferred = 0u64;
                let r = ((**task).execute_task_sync)(t, sense.as_mut_ptr(), &mut status, &mut transferred);
                if r != 0 {
                    return Err(Error::Io(std::io::Error::other(format!("SCSI task failed (0x{r:x})"))));
                }
                let sense = if status != 0 { Sense::parse(&sense) } else { None };
                Ok(Completion { status: status as u8, transferred: transferred as usize, sense })
            })();
            ((**task).release)(t);
            result
        }
    }

    fn describe(&self) -> String {
        format!("IOKit {}", self.address)
    }
}
