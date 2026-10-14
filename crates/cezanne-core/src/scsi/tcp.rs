//! SCSI over TCP, used to reach a scanner attached to another computer.
//!
//! `cezanne-bridge` runs next to the SCSI card and serves one scanner. Any Cezanne Scan
//! build (and the only option on macOS) connects with [`TcpTransport`].
//!
//! Wire format, all integers big endian:
//!
//! ```text
//! hello    client: "CZB1" u16 token_len token
//!          server: "CZB1" u8 ok(0)/refused(1) u16 desc_len desc
//! request  u8 1 | u8 cdb_len cdb | u8 dir (0 none, 1 in, 2 out) | u32 data_len | u32 timeout_ms | out data
//! response u8 status | u32 transferred | u8 sense_len sense | in data (transferred bytes)
//! ```
//!
//! The server runs [`super::check_allowed`] on every CDB, so a bridge can never be used to
//! flash firmware or rewrite EEPROM, whatever the client sends.

use super::{check_allowed, Completion, DataPhase, Sense, Transport};
use crate::error::{Error, Result};
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

pub const MAGIC: &[u8; 4] = b"CZB1";
pub const DEFAULT_PORT: u16 = 7810;
const MAX_DATA: usize = 64 * 1024 * 1024;

pub struct TcpTransport {
    stream: TcpStream,
    peer: String,
    remote_desc: String,
}

impl TcpTransport {
    pub fn connect(addr: &str, token: &str) -> Result<Self> {
        let with_port = if addr.contains(':') { addr.to_string() } else { format!("{addr}:{DEFAULT_PORT}") };
        let sa = with_port
            .to_socket_addrs()?
            .next()
            .ok_or_else(|| Error::NotFound(format!("cannot resolve {with_port}")))?;
        let mut stream = TcpStream::connect_timeout(&sa, Duration::from_secs(5))?;
        stream.set_nodelay(true)?;
        stream.write_all(MAGIC)?;
        write_bytes16(&mut stream, token.as_bytes())?;
        let mut magic = [0u8; 4];
        stream.read_exact(&mut magic)?;
        if &magic != MAGIC {
            return Err(Error::Protocol("not a cezanne-bridge".into()));
        }
        let ok = read_u8(&mut stream)?;
        let desc = String::from_utf8_lossy(&read_bytes16(&mut stream)?).to_string();
        if ok != 0 {
            return Err(Error::Protocol(format!("bridge refused the connection: {desc}")));
        }
        Ok(TcpTransport { stream, peer: with_port, remote_desc: desc })
    }
}

impl Transport for TcpTransport {
    fn execute(&mut self, cdb: &[u8], data: DataPhase<'_>, timeout: Duration) -> Result<Completion> {
        check_allowed(cdb)?;
        let s = &mut self.stream;
        s.set_read_timeout(Some(timeout + Duration::from_secs(10)))?;
        let mut req = Vec::with_capacity(16 + cdb.len());
        req.push(1);
        req.push(cdb.len() as u8);
        req.extend_from_slice(cdb);
        let (dir, len) = match &data {
            DataPhase::None => (0u8, 0usize),
            DataPhase::In(b) => (1, b.len()),
            DataPhase::Out(b) => (2, b.len()),
        };
        req.push(dir);
        req.extend_from_slice(&(len as u32).to_be_bytes());
        req.extend_from_slice(&(timeout.as_millis().min(u32::MAX as u128) as u32).to_be_bytes());
        s.write_all(&req)?;
        if let DataPhase::Out(b) = &data {
            s.write_all(b)?;
        }
        let status = read_u8(s)?;
        let transferred = read_u32(s)? as usize;
        let sense_len = read_u8(s)? as usize;
        let mut sense = vec![0u8; sense_len];
        s.read_exact(&mut sense)?;
        if let DataPhase::In(buf) = data {
            if transferred > buf.len() {
                return Err(Error::Protocol("bridge sent more data than requested".into()));
            }
            s.read_exact(&mut buf[..transferred])?;
        }
        Ok(Completion { status, transferred, sense: Sense::parse(&sense) })
    }

    fn describe(&self) -> String {
        format!("bridge {} ({})", self.peer, self.remote_desc)
    }
}

/// Serve one client connection until it disconnects. Used by `cezanne-bridge`.
pub fn serve(mut s: TcpStream, transport: &mut dyn Transport, token: &str) -> Result<()> {
    s.set_nodelay(true)?;
    let mut magic = [0u8; 4];
    s.read_exact(&mut magic)?;
    if &magic != MAGIC {
        return Err(Error::Protocol("client did not send the bridge greeting".into()));
    }
    let client_token = read_bytes16(&mut s)?;
    s.write_all(MAGIC)?;
    if client_token != token.as_bytes() {
        s.write_all(&[1])?;
        write_bytes16(&mut s, b"wrong token")?;
        return Err(Error::Protocol("client sent a wrong token".into()));
    }
    s.write_all(&[0])?;
    write_bytes16(&mut s, transport.describe().as_bytes())?;

    loop {
        let kind = match read_u8(&mut s) {
            Ok(k) => k,
            Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(e) => return Err(e),
        };
        if kind != 1 {
            return Err(Error::Protocol(format!("unknown request kind {kind}")));
        }
        let cdb_len = read_u8(&mut s)? as usize;
        let mut cdb = vec![0u8; cdb_len];
        s.read_exact(&mut cdb)?;
        let dir = read_u8(&mut s)?;
        let len = read_u32(&mut s)? as usize;
        let timeout = Duration::from_millis(read_u32(&mut s)? as u64);
        if len > MAX_DATA {
            return Err(Error::Protocol("data phase too large".into()));
        }
        let mut buf = vec![0u8; len];
        if dir == 2 {
            s.read_exact(&mut buf)?;
        }

        let result = if cdb.is_empty() {
            Err(Error::InvalidParameter("empty CDB".into()))
        } else if let Err(e) = check_allowed(&cdb) {
            log::warn!("refused command from client: {e}");
            Err(e)
        } else {
            let phase = match dir {
                0 => DataPhase::None,
                1 => DataPhase::In(&mut buf),
                _ => DataPhase::Out(&buf),
            };
            transport.execute(&cdb, phase, timeout)
        };
        let c = result.unwrap_or_else(|e| {
            log::warn!("command 0x{:02X} failed locally: {e}", cdb.first().copied().unwrap_or(0));
            Completion::check(Sense::new(super::SenseKey::IllegalRequest, 0x26, 0x02))
        });

        let sense = c.sense.as_ref().map(|x| x.to_bytes().to_vec()).unwrap_or_default();
        let transferred = if dir == 1 { c.transferred.min(len) } else { c.transferred };
        let mut resp = Vec::with_capacity(10 + sense.len());
        resp.push(c.status);
        resp.extend_from_slice(&(transferred as u32).to_be_bytes());
        resp.push(sense.len() as u8);
        resp.extend_from_slice(&sense);
        s.write_all(&resp)?;
        if dir == 1 {
            s.write_all(&buf[..transferred])?;
        }
    }
}

fn read_u8(r: &mut impl Read) -> Result<u8> {
    let mut b = [0u8; 1];
    r.read_exact(&mut b)?;
    Ok(b[0])
}

fn read_u32(r: &mut impl Read) -> Result<u32> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b)?;
    Ok(u32::from_be_bytes(b))
}

fn read_bytes16(r: &mut impl Read) -> Result<Vec<u8>> {
    let mut b = [0u8; 2];
    r.read_exact(&mut b)?;
    let mut v = vec![0u8; u16::from_be_bytes(b) as usize];
    r.read_exact(&mut v)?;
    Ok(v)
}

fn write_bytes16(w: &mut impl Write, b: &[u8]) -> Result<()> {
    let len = u16::try_from(b.len()).map_err(|_| Error::Protocol("string too long".into()))?;
    w.write_all(&len.to_be_bytes())?;
    w.write_all(b)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{cdb, Inquiry};
    use crate::scsi::sim::Simulator;
    use std::net::TcpListener;

    fn bridge(token: &'static str) -> String {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap().to_string();
        std::thread::spawn(move || {
            let (s, _) = l.accept().unwrap();
            let mut sim = Simulator::ready();
            let _ = serve(s, &mut sim, token);
        });
        addr
    }

    #[test]
    fn inquiry_over_the_bridge() {
        let addr = bridge("secret");
        let mut t = TcpTransport::connect(&addr, "secret").unwrap();
        let mut buf = [0u8; 36];
        let c = t.execute(&cdb::inquiry(false, 0, 36), DataPhase::In(&mut buf), Duration::from_secs(5)).unwrap();
        assert_eq!(c.status, 0);
        assert_eq!(Inquiry::parse(&buf).unwrap().product, "FT-S5000");
    }

    #[test]
    fn wrong_token_is_refused() {
        let addr = bridge("secret");
        assert!(TcpTransport::connect(&addr, "nope").is_err());
    }

    #[test]
    fn bridge_refuses_firmware_writes_even_from_a_raw_client() {
        let addr = bridge("");
        let mut s = TcpStream::connect(&addr).unwrap();
        s.write_all(MAGIC).unwrap();
        write_bytes16(&mut s, b"").unwrap();
        let mut hello = [0u8; 5];
        s.read_exact(&mut hello).unwrap();
        let _ = read_bytes16(&mut s).unwrap();
        // raw WRITE BUFFER request, bypassing the client side check
        let mut req = vec![1u8, 10, 0x3B, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        req.extend_from_slice(&0u32.to_be_bytes());
        req.extend_from_slice(&1000u32.to_be_bytes());
        s.write_all(&req).unwrap();
        let status = read_u8(&mut s).unwrap();
        assert_eq!(status, crate::scsi::status::CHECK_CONDITION);
    }
}
