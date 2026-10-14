//! CDB builders for every command the driver uses.

use super::put_be24;

pub const TEST_UNIT_READY: u8 = 0x00;
pub const REQUEST_SENSE: u8 = 0x03;
pub const INQUIRY: u8 = 0x12;
pub const RESERVE_UNIT: u8 = 0x16;
pub const RELEASE_UNIT: u8 = 0x17;
pub const SCAN: u8 = 0x1B;
pub const SEND_DIAGNOSTIC: u8 = 0x1D;
pub const SET_WINDOW: u8 = 0x24;
pub const GET_WINDOW: u8 = 0x25;
pub const READ_10: u8 = 0x28;
pub const SEND_10: u8 = 0x2A;
pub const LOG_SENSE: u8 = 0x4D;
pub const VENDOR_READ: u8 = 0xC8;
pub const VENDOR_MAINT_READ: u8 = 0xCA;
pub const VENDOR_WAIT: u8 = 0xCB;
pub const VENDOR_STATUS: u8 = 0xCE;

/// SEND(10) and vendor READ data type codes.
pub mod data_type {
    pub const GAMMA: u8 = 0x03;
    pub const CCD_DATA: u8 = 0x80;
    pub const RGB_MATRIX: u8 = 0x84;
}

pub fn test_unit_ready() -> [u8; 6] {
    [TEST_UNIT_READY, 0, 0, 0, 0, 0]
}

pub fn request_sense(len: u8) -> [u8; 6] {
    [REQUEST_SENSE, 0, 0, 0, len, 0]
}

pub fn inquiry(evpd: bool, page: u8, len: u8) -> [u8; 6] {
    [INQUIRY, evpd as u8, page, 0, len, 0]
}

pub fn reserve_unit() -> [u8; 6] {
    [RESERVE_UNIT, 0, 0, 0, 0, 0]
}

pub fn release_unit() -> [u8; 6] {
    [RELEASE_UNIT, 0, 0, 0, 0, 0]
}

/// SCAN: `count` window IDs follow in the data out phase.
pub fn scan(count: u8) -> [u8; 6] {
    [SCAN, 0, 0, 0, count, 0]
}

pub fn set_window(param_len: u32) -> [u8; 10] {
    let mut c = [0u8; 10];
    c[0] = SET_WINDOW;
    put_be24(&mut c[6..9], param_len);
    c
}

pub fn get_window(window_id: u8, alloc_len: u32) -> [u8; 10] {
    let mut c = [0u8; 10];
    c[0] = GET_WINDOW;
    c[5] = window_id;
    put_be24(&mut c[6..9], alloc_len);
    c
}

pub fn read10(len: u32) -> [u8; 10] {
    let mut c = [0u8; 10];
    c[0] = READ_10;
    put_be24(&mut c[6..9], len);
    c
}

/// SEND(10). The save to EEPROM bit is deliberately not exposed.
pub fn send10(dtc: u8, qualifier: u8, len: u32) -> [u8; 10] {
    let mut c = [0u8; 10];
    c[0] = SEND_10;
    c[2] = dtc;
    c[5] = qualifier;
    put_be24(&mut c[6..9], len);
    c
}

/// Vendor READ (0xC8): auxiliary data such as gamma tables or raw CCD lines.
pub fn vendor_read(dtc: u8, qualifier: u16, len: u32) -> [u8; 10] {
    let mut c = [0u8; 10];
    c[0] = VENDOR_READ;
    c[2] = dtc;
    c[4..6].copy_from_slice(&qualifier.to_be_bytes());
    put_be24(&mut c[6..9], len);
    c
}

/// Vendor maintenance read (0xCA): read only diagnostics.
pub fn maint_read(function: u16, sub: u16, len: u32) -> [u8; 10] {
    let mut c = [0u8; 10];
    c[0] = VENDOR_MAINT_READ;
    c[2..4].copy_from_slice(&function.to_be_bytes());
    c[4..6].copy_from_slice(&sub.to_be_bytes());
    put_be24(&mut c[6..9], len);
    c
}

/// Vendor wait (0xCB): returns when the current mechanical move has finished.
pub fn vendor_wait() -> [u8; 10] {
    let mut c = [0u8; 10];
    c[0] = VENDOR_WAIT;
    c
}

/// Vendor status (0xCE): 6 byte status block.
pub fn vendor_status(len: u32) -> [u8; 10] {
    let mut c = [0u8; 10];
    c[0] = VENDOR_STATUS;
    put_be24(&mut c[6..9], len);
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn length_fields_are_big_endian_24_bit() {
        let c = read10(0x012345);
        assert_eq!(&c[6..9], &[0x01, 0x23, 0x45]);
        let c = set_window(8 + 176);
        assert_eq!(&c[6..9], &[0, 0, 184]);
    }

    #[test]
    fn send_never_sets_save_bit() {
        let c = send10(data_type::GAMMA, 1, 1530);
        assert_eq!(c[4] & 1, 0);
        assert!(crate::scsi::check_allowed(&c).is_ok());
    }
}
