use crate::scsi::Sense;

/// Errors from any layer of the driver.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// The device finished a command with CHECK CONDITION or another bad status.
    #[error("SCSI command 0x{opcode:02X} failed with status 0x{status:02X}{}", sense.as_ref().map(|s| format!(": {s}")).unwrap_or_default())]
    Scsi { opcode: u8, status: u8, sense: Option<Sense> },

    #[error("protocol error: {0}")]
    Protocol(String),

    #[error("invalid parameter: {0}")]
    InvalidParameter(String),

    /// A command was refused because it could damage the scanner (firmware or EEPROM writes).
    #[error("blocked unsafe command: {0}")]
    Blocked(String),

    #[error("timed out: {0}")]
    Timeout(String),

    #[error("cancelled")]
    Cancelled,

    #[error("no scanner found: {0}")]
    NotFound(String),

    #[error("not supported on this platform: {0}")]
    Unsupported(String),
}

pub type Result<T> = std::result::Result<T, Error>;
