//! Choosing and opening a connection, shared by the app, the CLI and the bridge.

use crate::error::{Error, Result};
use crate::scsi::sim::{SimConfig, Simulator};
use crate::scsi::tcp::TcpTransport;
use crate::scsi::{self, Transport};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Connection {
    /// The built in simulated scanner.
    Simulator,
    /// First supported scanner found on this computer.
    AutoLocal,
    /// A specific local device, for example `/dev/sg3` or `scsi2:0:5:0`.
    Local(String),
    /// A `cezanne-bridge` on another computer.
    Bridge { host: String, token: String },
}

impl Default for Connection {
    fn default() -> Self {
        if cfg!(target_os = "macos") {
            Connection::Bridge { host: String::new(), token: String::new() }
        } else {
            Connection::AutoLocal
        }
    }
}

impl Connection {
    pub fn open(&self) -> Result<Box<dyn Transport>> {
        self.open_with(SimConfig::default())
    }

    pub fn open_with(&self, sim: SimConfig) -> Result<Box<dyn Transport>> {
        match self {
            Connection::Simulator => Ok(Box::new(Simulator::new(sim))),
            Connection::AutoLocal => {
                let found = scsi::discover()?;
                let first = found
                    .first()
                    .ok_or_else(|| Error::NotFound("no SCREEN scanner found on the SCSI bus".into()))?;
                log::info!("using {} {} at {}", first.vendor, first.product, first.address);
                scsi::open_local(&first.address)
            }
            Connection::Local(addr) => scsi::open_local(addr),
            Connection::Bridge { host, token } => {
                if host.trim().is_empty() {
                    return Err(Error::InvalidParameter("enter the bridge host name or address".into()));
                }
                Ok(Box::new(TcpTransport::connect(host.trim(), token)?))
            }
        }
    }

    pub fn label(&self) -> String {
        match self {
            Connection::Simulator => "Simulator".into(),
            Connection::AutoLocal => "This computer (automatic)".into(),
            Connection::Local(a) => format!("This computer, {a}"),
            Connection::Bridge { host, .. } => format!("Bridge {host}"),
        }
    }
}
