//! A simulated FT-S5000.
//!
//! It answers the command set the firmware implements, applies the same window
//! validation and returns synthetic film on a virtual bed: a colour negative with its
//! orange mask, a black and white negative and a colour slide with a step wedge. The app,
//! the CLI and the tests all run against it when no scanner is attached.

use super::{check_allowed, Completion, DataPhase, Sense, SenseKey, Transport};
use crate::error::Result;
use crate::protocol::window::{AreaUnit, ModelLimits, WindowDescriptor, DESCRIPTOR_LEN, HEADER_LEN};
use crate::protocol::{be24, cdb, gamma, Geometry, Inquiry};
use std::collections::{HashMap, VecDeque};
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct SimConfig {
    /// TEST UNIT READY answers NOT READY this many times after power on (lamp warm up).
    pub warmup_polls: u32,
    /// Report a power on UNIT ATTENTION on the first command.
    pub power_on_attention: bool,
    /// Sleep per delivered line, to make progress bars realistic in the app.
    pub line_delay: Duration,
    pub revision: String,
    /// SCAN answers NOT READY 04/80 this many times (lamp warm up seen on SCAN, as
    /// SCREEN's driver expects).
    pub scan_warmup: u32,
}

impl Default for SimConfig {
    fn default() -> Self {
        SimConfig {
            warmup_polls: 2,
            power_on_attention: true,
            line_delay: Duration::ZERO,
            revision: "2.12".into(),
            scan_warmup: 0,
        }
    }
}

struct Job {
    desc: WindowDescriptor,
    geometry: Geometry,
    next_line: u32,
    pending: VecDeque<u8>,
}

pub struct Simulator {
    cfg: SimConfig,
    warmup_left: u32,
    scan_warmup_left: u32,
    attention: bool,
    sense: Option<Sense>,
    header: [u8; HEADER_LEN],
    windows: HashMap<u8, WindowDescriptor>,
    gamma: HashMap<u8, Vec<u8>>,
    matrix: HashMap<u8, Vec<u8>>,
    jobs: VecDeque<Job>,
    reserved: bool,
    /// Every CDB received, for tests.
    pub log: Vec<Vec<u8>>,
}

impl Default for Simulator {
    fn default() -> Self {
        Self::new(SimConfig::default())
    }
}

impl Simulator {
    pub fn new(cfg: SimConfig) -> Self {
        Simulator {
            warmup_left: cfg.warmup_polls,
            scan_warmup_left: cfg.scan_warmup,
            attention: cfg.power_on_attention,
            cfg,
            sense: None,
            header: [0; HEADER_LEN],
            windows: HashMap::new(),
            gamma: HashMap::new(),
            matrix: HashMap::new(),
            jobs: VecDeque::new(),
            reserved: false,
            log: Vec::new(),
        }
    }

    /// No warm up, no unit attention: handy for tests.
    pub fn ready() -> Self {
        Self::new(SimConfig { warmup_polls: 0, power_on_attention: false, ..SimConfig::default() })
    }

    fn fail(&mut self, key: SenseKey, asc: u8, ascq: u8) -> Completion {
        let s = Sense::new(key, asc, ascq);
        self.sense = Some(s.clone());
        Completion::check(s)
    }

    fn illegal(&mut self, asc: u8, ascq: u8) -> Completion {
        self.fail(SenseKey::IllegalRequest, asc, ascq)
    }

    fn handle(&mut self, c: &[u8], data: DataPhase<'_>) -> Completion {
        let op = c[0];
        if self.attention && op != cdb::INQUIRY && op != cdb::REQUEST_SENSE {
            self.attention = false;
            return self.fail(SenseKey::UnitAttention, 0x29, 0x00);
        }
        if check_allowed(c).is_err() {
            return self.illegal(0x26, 0x02);
        }
        match op {
            cdb::TEST_UNIT_READY => {
                if self.warmup_left > 0 {
                    self.warmup_left -= 1;
                    return self.fail(SenseKey::NotReady, 0x04, 0x01);
                }
                Completion::good(0)
            }
            cdb::REQUEST_SENSE => {
                let s = self.sense.take().unwrap_or_else(|| Sense::new(SenseKey::NoSense, 0, 0));
                give(data, &s.to_bytes())
            }
            cdb::INQUIRY => {
                if c[1] & 1 != 0 {
                    // vendor page: page code then ten bytes of serial number
                    let mut p = vec![0x06, c[2], 0, 10];
                    p.extend_from_slice(b"SIM0000001");
                    give(data, &p)
                } else {
                    give(data, &Inquiry::ft_s5000_bytes(&self.cfg.revision))
                }
            }
            cdb::RESERVE_UNIT => {
                self.reserved = true;
                Completion::good(0)
            }
            cdb::RELEASE_UNIT => {
                self.reserved = false;
                Completion::good(0)
            }
            cdb::SEND_DIAGNOSTIC | cdb::VENDOR_WAIT => Completion::good(0),
            cdb::SET_WINDOW => self.set_window(c, data),
            cdb::GET_WINDOW => self.get_window(c, data),
            cdb::SCAN => self.scan(c, data),
            cdb::READ_10 => self.read(c, data),
            cdb::SEND_10 => self.send(c, data),
            cdb::VENDOR_READ => self.vendor_read(c, data),
            cdb::VENDOR_STATUS => {
                let busy = !self.jobs.is_empty();
                give(data, &[0, 6, busy as u8, 0, 0, 0])
            }
            cdb::LOG_SENSE => give(data, &[c[2] & 0x3F, 0, 0, 0]),
            _ => self.illegal(0x20, 0x00),
        }
    }

    fn set_window(&mut self, c: &[u8], data: DataPhase<'_>) -> Completion {
        let len = be24(&c[6..9]) as usize;
        if len > 0x5000 {
            return self.illegal(0x24, 0x00);
        }
        let DataPhase::Out(buf) = data else { return self.illegal(0x24, 0x00) };
        if buf.len() < HEADER_LEN {
            return self.illegal(0x26, 0x02);
        }
        self.header.copy_from_slice(&buf[..HEADER_LEN]);
        for chunk in buf[HEADER_LEN..].chunks(DESCRIPTOR_LEN) {
            if chunk.len() < DESCRIPTOR_LEN {
                break;
            }
            let d = WindowDescriptor::from_bytes(chunk).expect("length checked");
            if d.window_id() >= 200 || d.validate(&ModelLimits::FT_S5000).is_err() {
                return self.illegal(0x26, 0x02);
            }
            self.windows.insert(d.window_id(), d);
        }
        Completion::good(buf.len())
    }

    fn get_window(&mut self, c: &[u8], data: DataPhase<'_>) -> Completion {
        let id = c[5];
        let d = self.windows.get(&id).cloned().unwrap_or_else(|| {
            let mut z = WindowDescriptor::zeroed();
            z.0[0] = id;
            z
        });
        let mut out = self.header.to_vec();
        out.extend_from_slice(&d.0);
        give(data, &out)
    }

    fn scan(&mut self, c: &[u8], data: DataPhase<'_>) -> Completion {
        if self.scan_warmup_left > 0 {
            self.scan_warmup_left -= 1;
            return self.fail(SenseKey::NotReady, 0x04, 0x80);
        }
        let count = c[4] as usize;
        if count > 100 {
            return self.illegal(0x26, 0x02);
        }
        let ids: Vec<u8> = match data {
            DataPhase::Out(b) if b.len() >= count => b[..count].to_vec(),
            _ if count == 0 => vec![0],
            _ => return self.illegal(0x26, 0x02),
        };
        let mut jobs = VecDeque::new();
        for id in ids {
            let Some(desc) = self.windows.get(&id).cloned() else { return self.illegal(0x26, 0x02) };
            let Some(geometry) = desc.geometry() else { return self.illegal(0x26, 0x02) };
            jobs.push_back(Job { desc, geometry, next_line: 0, pending: VecDeque::new() });
        }
        self.jobs = jobs;
        Completion::good(count)
    }

    fn read(&mut self, c: &[u8], data: DataPhase<'_>) -> Completion {
        let want = be24(&c[6..9]) as usize;
        let DataPhase::In(buf) = data else { return self.illegal(0x24, 0x00) };
        let want = want.min(buf.len());
        let delay = self.cfg.line_delay;
        let Some(job) = self.jobs.front_mut() else { return self.illegal(0x2C, 0x00) };
        while job.pending.len() < want && job.next_line < job.geometry.lines {
            let line = render_line(&job.desc, &job.geometry, job.next_line);
            job.pending.extend(line);
            job.next_line += 1;
            if !delay.is_zero() {
                std::thread::sleep(delay);
            }
        }
        let n = want.min(job.pending.len());
        for (d, s) in buf[..n].iter_mut().zip(job.pending.drain(..n)) {
            *d = s;
        }
        if job.pending.is_empty() && job.next_line >= job.geometry.lines {
            self.jobs.pop_front();
        }
        if n == 0 {
            return self.illegal(0x2C, 0x00);
        }
        Completion::good(n)
    }

    fn send(&mut self, c: &[u8], data: DataPhase<'_>) -> Completion {
        let dtc = c[2];
        let q = c[5];
        let DataPhase::Out(buf) = data else { return self.illegal(0x24, 0x00) };
        match dtc {
            cdb::data_type::GAMMA if gamma::SLOTS.contains(&q) => {
                self.gamma.insert(q, buf.to_vec());
                Completion::good(buf.len())
            }
            cdb::data_type::RGB_MATRIX if (1..=4).contains(&q) && buf.len() == 60 => {
                self.matrix.insert(q, buf.to_vec());
                Completion::good(buf.len())
            }
            _ => self.illegal(0x26, 0x02),
        }
    }

    fn vendor_read(&mut self, c: &[u8], data: DataPhase<'_>) -> Completion {
        let q = u16::from_be_bytes([c[4], c[5]]) as u8;
        match c[2] {
            cdb::data_type::GAMMA => {
                let t = self.gamma.get(&q).cloned().unwrap_or_else(|| gamma::GammaTable::identity().to_bytes());
                give(data, &t)
            }
            cdb::data_type::RGB_MATRIX => {
                let m = self.matrix.get(&q).cloned().unwrap_or_else(|| vec![0; 60]);
                give(data, &m)
            }
            _ => self.illegal(0x26, 0x02),
        }
    }
}

impl Transport for Simulator {
    fn execute(&mut self, c: &[u8], data: DataPhase<'_>, _timeout: Duration) -> Result<Completion> {
        self.log.push(c.to_vec());
        if c.is_empty() {
            return Ok(self.illegal(0x20, 0x00));
        }
        Ok(self.handle(c, data))
    }

    fn describe(&self) -> String {
        "simulated FT-S5000".into()
    }
}

fn give(data: DataPhase<'_>, src: &[u8]) -> Completion {
    match data {
        DataPhase::In(buf) => {
            let n = buf.len().min(src.len());
            buf[..n].copy_from_slice(&src[..n]);
            Completion::good(n)
        }
        _ => Completion::good(0),
    }
}

// ---------------------------------------------------------------------------------
// Synthetic bed

/// Light transmission (0 to 1) of the virtual bed at a point, per channel.
pub fn bed_transmission(x: f64, y: f64) -> [f64; 3] {
    // colour negative, 6x7 frame
    if let Some((u, v)) = inside(x, y, 30.0, 30.0, 70.0, 56.0) {
        let scene = scene_rgb(u, v);
        let base = [0.78, 0.48, 0.26]; // orange mask
        let mut t = [0.0; 3];
        for c in 0..3 {
            // negative: denser where the scene is brighter
            let density = 0.65 * scene[c].max(0.004).log10().abs().min(2.4);
            let d = (2.0 - density).max(0.0) * 0.6 + 0.05;
            t[c] = base[c] * 10f64.powf(-d);
        }
        return t;
    }
    // black and white negative, 6x9 frame
    if let Some((u, v)) = inside(x, y, 120.0, 30.0, 84.0, 56.0) {
        let s = scene_rgb(u, v);
        let lum = 0.3 * s[0] + 0.59 * s[1] + 0.11 * s[2];
        let d = 0.15 + 1.6 * lum.powf(0.8);
        let t = 0.92 * 10f64.powf(-d);
        return [t; 3];
    }
    // colour slide with a 21 step wedge underneath
    if let Some((u, v)) = inside(x, y, 30.0, 110.0, 56.0, 42.0) {
        let s = scene_rgb(u, v);
        return [s[0] * 0.9, s[1] * 0.9, s[2] * 0.9];
    }
    if let Some((u, _)) = inside(x, y, 30.0, 160.0, 160.0, 12.0) {
        let step = (u * 21.0).floor().min(20.0);
        let t = 10f64.powf(-0.15 * step);
        return [t; 3];
    }
    // clear glass
    [0.93, 0.93, 0.93]
}

fn inside(x: f64, y: f64, x0: f64, y0: f64, w: f64, h: f64) -> Option<(f64, f64)> {
    if x >= x0 && x < x0 + w && y >= y0 && y < y0 + h {
        Some(((x - x0) / w, (y - y0) / h))
    } else {
        None
    }
}

/// A small landscape: sky, sun, hills and colour patches. Linear light, 0 to 1.
fn scene_rgb(u: f64, v: f64) -> [f64; 3] {
    let horizon = 0.55 + 0.08 * (u * 6.0).sin();
    let sun = ((u - 0.72).powi(2) + (v - 0.22).powi(2)).sqrt();
    if sun < 0.08 {
        return [1.0, 0.95, 0.75];
    }
    if v < horizon {
        let k = v / horizon;
        return [0.25 + 0.35 * k, 0.45 + 0.3 * k, 0.9 - 0.25 * k];
    }
    // colour checker strip along the bottom
    if v > 0.86 {
        let patch = (u * 6.0).floor() as usize;
        let p = [[0.6, 0.1, 0.1], [0.1, 0.5, 0.1], [0.1, 0.1, 0.6], [0.7, 0.7, 0.1], [0.5, 0.5, 0.5], [0.05, 0.05, 0.05]];
        return p[patch.min(5)];
    }
    let k = (v - horizon) / (1.0 - horizon);
    [0.12 + 0.1 * k, 0.35 - 0.15 * k, 0.08]
}

/// Render one scan line as the scanner would send it (big endian samples).
pub fn render_line(desc: &WindowDescriptor, g: &Geometry, line: u32) -> Vec<u8> {
    let per_mm = AreaUnit::TenthMm.units_per_mm();
    let x0 = desc.upper_left_x() as f64 / per_mm;
    let y0 = desc.upper_left_y() as f64 / per_mm;
    // like the real scanner, the extent follows from pixel count and resolution
    let (ew, eh) = desc.extent();
    let w = ew as f64 / per_mm;
    let h = eh as f64 / per_mm;
    let y = y0 + (line as f64 + 0.5) / g.lines as f64 * h;
    let mut out = Vec::with_capacity(g.bytes_per_line());
    let channels = g.mode.channels();
    let sixteen = g.mode.bytes_per_sample() == 2;
    for p in 0..g.pixels_per_line {
        let x = x0 + (p as f64 + 0.5) / g.pixels_per_line as f64 * w;
        let t = bed_transmission(x, y);
        let n = noise(p, line) * 0.002;
        let vals: Vec<f64> = if channels == 3 {
            t.iter().map(|v| (v + n).clamp(0.0, 1.0)).collect()
        } else {
            vec![((0.3 * t[0] + 0.59 * t[1] + 0.11 * t[2]) + n).clamp(0.0, 1.0)]
        };
        for v in vals {
            if sixteen {
                out.extend_from_slice(&((v * 65535.0).round() as u16).to_be_bytes());
            } else {
                // 8 bit output is gamma encoded, as on a real scanner
                out.push((v.powf(1.0 / 2.2) * 255.0).round() as u8);
            }
        }
    }
    out
}

fn noise(x: u32, y: u32) -> f64 {
    let mut h = x.wrapping_mul(0x9E37_79B1) ^ y.wrapping_mul(0x85EB_CA77);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2C1B_3C6D);
    h ^= h >> 12;
    (h & 0xFFFF) as f64 / 65535.0 - 0.5
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::window::{parameter_list, ColorMode, ScanArea, WindowSettings};
    use crate::scsi::status;

    fn exec(s: &mut Simulator, c: &[u8], d: DataPhase<'_>) -> Completion {
        s.execute(c, d, Duration::from_secs(1)).unwrap()
    }

    #[test]
    fn warmup_then_ready() {
        let mut s = Simulator::default();
        let c = cdb::test_unit_ready();
        assert_eq!(exec(&mut s, &c, DataPhase::None).sense.unwrap().key, SenseKey::UnitAttention);
        assert_eq!(exec(&mut s, &c, DataPhase::None).sense.unwrap().key, SenseKey::NotReady);
        assert_eq!(exec(&mut s, &c, DataPhase::None).sense.unwrap().key, SenseKey::NotReady);
        assert_eq!(exec(&mut s, &c, DataPhase::None).status, status::GOOD);
    }

    #[test]
    fn unknown_opcode_is_illegal() {
        let mut s = Simulator::ready();
        let r = exec(&mut s, &[0x08, 0, 0, 0, 0, 0], DataPhase::None);
        let sense = r.sense.unwrap();
        assert_eq!((sense.key, sense.asc), (SenseKey::IllegalRequest, 0x20));
    }

    #[test]
    fn full_scan_delivers_exact_byte_count() {
        let mut s = Simulator::ready();
        let ws = WindowSettings::new(ScanArea::new(30.0, 30.0, 20.0, 10.0), 300.0, ColorMode::Rgb48);
        let (d, g) = WindowDescriptor::build(&ws).unwrap();
        let p = parameter_list(&[d.clone()]);
        assert_eq!(exec(&mut s, &cdb::set_window(p.len() as u32), DataPhase::Out(&p)).status, 0);
        assert_eq!(exec(&mut s, &cdb::scan(1), DataPhase::Out(&[d.window_id()])).status, 0);
        let mut total = 0;
        let mut buf = vec![0u8; 4096];
        loop {
            let r = exec(&mut s, &cdb::read10(4096), DataPhase::In(&mut buf));
            if r.status != 0 {
                break;
            }
            total += r.transferred;
        }
        assert_eq!(total, g.total_bytes());
    }
}
