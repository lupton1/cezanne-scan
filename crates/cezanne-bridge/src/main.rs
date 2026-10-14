//! `cezanne-bridge`: share one SCREEN scanner over the network.
//!
//! Run it on the computer with the SCSI card (Linux or Windows). Cezanne Scan on any
//! other computer, including current Macs which have no SCSI support, connects to it.
//! One client is served at a time. Firmware and EEPROM writes are always refused.

use cezanne_core::connect::Connection;
use cezanne_core::scsi::tcp::{serve, DEFAULT_PORT};
use clap::Parser;
use std::net::TcpListener;

#[derive(Parser)]
#[command(name = "cezanne-bridge", version, about = "Share a SCREEN Cezanne scanner over the network")]
struct Args {
    /// Address and port to listen on. Use 0.0.0.0 to accept other computers on your network.
    #[arg(long, default_value_t = format!("127.0.0.1:{DEFAULT_PORT}"))]
    listen: String,
    /// Local device address (`/dev/sg3`, `scsi2:0:5:0`). Default: first SCREEN scanner found.
    #[arg(long)]
    device: Option<String>,
    /// Shared secret clients must send. Strongly recommended when listening on the network.
    #[arg(long, env = "CEZANNE_BRIDGE_TOKEN", default_value = "")]
    token: String,
    /// Serve the simulated scanner instead of real hardware (for testing a Mac setup).
    #[arg(long)]
    sim: bool,
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let a = Args::parse();
    let conn = if a.sim {
        Connection::Simulator
    } else if let Some(d) = &a.device {
        Connection::Local(d.clone())
    } else {
        Connection::AutoLocal
    };
    if a.token.is_empty() && !a.listen.starts_with("127.") && !a.listen.starts_with("localhost") {
        log::warn!("listening on the network without --token: anyone on your network can use the scanner");
    }
    let listener = match TcpListener::bind(&a.listen) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("cannot listen on {}: {e}", a.listen);
            std::process::exit(1);
        }
    };
    log::info!("cezanne-bridge {} listening on {}", env!("CARGO_PKG_VERSION"), a.listen);
    for stream in listener.incoming() {
        let stream = match stream {
            Ok(s) => s,
            Err(e) => {
                log::warn!("accept failed: {e}");
                continue;
            }
        };
        let peer = stream.peer_addr().map(|p| p.to_string()).unwrap_or_default();
        // open the device per client, so a scanner switched on later is picked up
        let mut transport = match conn.open() {
            Ok(t) => t,
            Err(e) => {
                log::error!("cannot open the scanner for {peer}: {e}");
                continue;
            }
        };
        log::info!("client {peer} connected, serving {}", transport.describe());
        match serve(stream, transport.as_mut(), &a.token) {
            Ok(()) => log::info!("client {peer} disconnected"),
            Err(e) => log::warn!("client {peer}: {e}"),
        }
    }
}
