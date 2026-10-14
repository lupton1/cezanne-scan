//! Core library for Cezanne Scan.
//!
//! * [`scsi`] moves raw SCSI commands to the scanner through a [`scsi::Transport`]
//!   (Linux SG_IO, Windows SPTI, a TCP bridge, or the built in simulator).
//! * [`protocol`] builds and parses the FT-S5000 commands and data structures.
//! * [`scanner`] is the high level driver used by the app and the CLI.
//! * [`image`] turns raw scanner data into finished 16 bit TIFF files.

pub mod connect;
pub mod error;
pub mod image;
pub mod preset;
pub mod protocol;
pub mod scanner;
pub mod scsi;

pub use error::{Error, Result};
