//! High level FT-S5000 driver.

use crate::error::{Error, Result};
use crate::image::RawImage;
use crate::protocol::window::{parameter_list, ModelLimits, WindowDescriptor, DESCRIPTOR_LEN, HEADER_LEN};
use crate::protocol::{cdb, gamma, GammaTable, Geometry, Inquiry, WindowSettings};
use crate::scsi::{self, DataPhase, Sense, SenseKey, Transport};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const SHORT: Duration = Duration::from_secs(10);
/// SCAN can involve moving the optics and warming lamps.
const LONG: Duration = Duration::from_secs(120);
/// Target size of one READ(10).
const READ_CHUNK: usize = 256 * 1024;

/// Progress callback: fraction done 0 to 1.
pub type Progress<'a> = &'a mut dyn FnMut(f32);

pub struct Scanner {
    t: Box<dyn Transport>,
    pub inquiry: Inquiry,
    pub limits: ModelLimits,
    reserved: bool,
}

impl Scanner {
    /// Wrap a transport and identify the device.
    pub fn open(mut t: Box<dyn Transport>) -> Result<Scanner> {
        let mut buf = [0u8; 36];
        // INQUIRY is answered even while a unit attention is pending.
        scsi::run(t.as_mut(), &cdb::inquiry(false, 0, 36), DataPhase::In(&mut buf), SHORT)?;
        let inquiry = Inquiry::parse(&buf).ok_or_else(|| Error::Protocol("short INQUIRY data".into()))?;
        if !inquiry.is_supported() {
            return Err(Error::NotFound(format!(
                "{} {} is not a supported SCREEN scanner",
                inquiry.vendor, inquiry.product
            )));
        }
        let limits = ModelLimits::for_product(&inquiry.product);
        log::info!("opened {} {} rev {} via {}", inquiry.vendor, inquiry.product, inquiry.revision, t.describe());
        // Consume a pending power on UNIT ATTENTION so the next real command does not fail.
        // NOT READY (warm up) is fine here, wait_ready handles it before scanning.
        for _ in 0..2 {
            match scsi::run(t.as_mut(), &cdb::test_unit_ready(), DataPhase::None, SHORT) {
                Err(Error::Scsi { sense: Some(s), .. }) if s.key == SenseKey::UnitAttention => continue,
                _ => break,
            }
        }
        Ok(Scanner { t, inquiry, limits, reserved: false })
    }

    pub fn describe(&self) -> String {
        format!("{} {} {} ({})", self.inquiry.vendor, self.inquiry.product, self.inquiry.revision, self.t.describe())
    }

    fn run(&mut self, c: &[u8], data: DataPhase<'_>, timeout: Duration) -> Result<usize> {
        scsi::run(self.t.as_mut(), c, data, timeout)
    }

    /// Poll TEST UNIT READY until the scanner is ready (lamp warm up can take minutes).
    pub fn wait_ready(&mut self, max: Duration, cancel: &AtomicBool) -> Result<()> {
        let start = Instant::now();
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            match self.run(&cdb::test_unit_ready(), DataPhase::None, SHORT) {
                Ok(_) => return Ok(()),
                Err(Error::Scsi { sense: Some(s), .. }) if matches!(s.key, SenseKey::UnitAttention) => continue,
                Err(Error::Scsi { sense: Some(s), .. }) if matches!(s.key, SenseKey::NotReady) => {
                    if start.elapsed() > max {
                        return Err(Error::Timeout(format!("scanner not ready after {:?}: {s}", max)));
                    }
                    std::thread::sleep(Duration::from_millis(500));
                }
                Err(e) => return Err(e),
            }
        }
    }

    pub fn request_sense(&mut self) -> Result<Sense> {
        let mut b = [0u8; Sense::LEN];
        self.run(&cdb::request_sense(Sense::LEN as u8), DataPhase::In(&mut b), SHORT)?;
        Sense::parse(&b).ok_or_else(|| Error::Protocol("bad sense data".into()))
    }

    pub fn reserve(&mut self) -> Result<()> {
        self.run(&cdb::reserve_unit(), DataPhase::None, SHORT)?;
        self.reserved = true;
        Ok(())
    }

    pub fn release(&mut self) -> Result<()> {
        self.reserved = false;
        self.run(&cdb::release_unit(), DataPhase::None, SHORT).map(|_| ())
    }

    pub fn set_windows(&mut self, windows: &[WindowDescriptor]) -> Result<()> {
        for w in windows {
            w.validate(&self.limits)?;
        }
        let p = parameter_list(windows);
        self.run(&cdb::set_window(p.len() as u32), DataPhase::Out(&p), SHORT)?;
        Ok(())
    }

    pub fn get_window(&mut self, id: u8) -> Result<WindowDescriptor> {
        let mut b = vec![0u8; HEADER_LEN + DESCRIPTOR_LEN];
        let n = self.run(&cdb::get_window(id, b.len() as u32), DataPhase::In(&mut b), SHORT)?;
        if n < HEADER_LEN + DESCRIPTOR_LEN {
            return Err(Error::Protocol(format!("GET WINDOW returned {n} bytes")));
        }
        Ok(WindowDescriptor::from_bytes(&b[HEADER_LEN..]).expect("length checked"))
    }

    /// Download a gamma table into slot 1 to 6 (RAM only, never EEPROM).
    pub fn send_gamma(&mut self, slot: u8, table: &GammaTable) -> Result<()> {
        if !gamma::SLOTS.contains(&slot) {
            return Err(Error::InvalidParameter(format!("gamma slot {slot} must be 1 to 6")));
        }
        let b = table.to_bytes();
        self.run(&cdb::send10(cdb::data_type::GAMMA, slot, b.len() as u32), DataPhase::Out(&b), SHORT)?;
        Ok(())
    }

    pub fn vendor_status(&mut self) -> Result<[u8; 6]> {
        let mut b = [0u8; 6];
        self.run(&cdb::vendor_status(6), DataPhase::In(&mut b), SHORT)?;
        Ok(b)
    }

    /// Start a scan of the listed windows.
    pub fn start(&mut self, ids: &[u8]) -> Result<()> {
        if ids.is_empty() || ids.len() > 100 {
            return Err(Error::InvalidParameter("a scan needs 1 to 100 windows".into()));
        }
        self.run(&cdb::scan(ids.len() as u8), DataPhase::Out(ids), LONG)?;
        Ok(())
    }

    /// Read exactly one window's worth of image data.
    pub fn read_image(&mut self, g: &Geometry, progress: Progress<'_>, cancel: &AtomicBool) -> Result<Vec<u8>> {
        let total = g.total_bytes();
        let bpl = g.bytes_per_line().max(1);
        // whole lines per READ keeps the host and scanner buffers aligned
        let chunk = (READ_CHUNK / bpl).max(1) * bpl;
        let mut out = vec![0u8; total];
        let mut done = 0usize;
        while done < total {
            if cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            let want = chunk.min(total - done);
            let n = self.run(&cdb::read10(want as u32), DataPhase::In(&mut out[done..done + want]), LONG)?;
            if n == 0 {
                return Err(Error::Protocol(format!("scanner stopped sending after {done} of {total} bytes")));
            }
            done += n;
            progress(done as f32 / total as f32);
        }
        Ok(out)
    }

    /// One complete scan: set the window, start, read, decode.
    pub fn scan(&mut self, settings: &WindowSettings, progress: Progress<'_>, cancel: &AtomicBool) -> Result<RawImage> {
        let (desc, g) = WindowDescriptor::build(settings)?;
        self.wait_ready(Duration::from_secs(300), cancel)?;
        let reserved_here = !self.reserved;
        if reserved_here {
            self.reserve()?;
        }
        let result = (|| {
            self.set_windows(std::slice::from_ref(&desc))?;
            self.start(&[desc.window_id()])?;
            let bytes = self.read_image(&g, progress, cancel)?;
            RawImage::from_scanner(&g, &bytes, settings.x_dpi, settings.y_dpi)
        })();
        if reserved_here {
            if let Err(e) = self.release() {
                log::warn!("RELEASE UNIT failed: {e}");
            }
        }
        result
    }

    /// Low resolution scan of an area for the preview.
    pub fn preview(&mut self, settings: &WindowSettings, progress: Progress<'_>, cancel: &AtomicBool) -> Result<RawImage> {
        self.scan(settings, progress, cancel)
    }

    /// Read only identification and stored windows. Safe to run on a real scanner at any time.
    pub fn probe(&mut self) -> Result<String> {
        use std::fmt::Write;
        let mut r = String::new();
        let _ = writeln!(r, "Device: {}", self.describe());
        let _ = writeln!(r, "Inquiry vendor bytes: {:02x?}", self.inquiry.vendor_specific);
        let mut page = [0u8; 64];
        match self.run(&cdb::inquiry(true, 0, 64), DataPhase::In(&mut page), SHORT) {
            Ok(n) => {
                let _ = writeln!(r, "Vendor inquiry page: {:02x?}", &page[..n]);
            }
            Err(e) => {
                let _ = writeln!(r, "Vendor inquiry page: {e}");
            }
        }
        match self.vendor_status() {
            Ok(s) => {
                let _ = writeln!(r, "Vendor status (0xCE): {s:02x?}");
            }
            Err(e) => {
                let _ = writeln!(r, "Vendor status (0xCE): {e}");
            }
        }
        for id in 0..4u8 {
            match self.get_window(id) {
                Ok(w) => {
                    let _ = writeln!(r, "\nWindow {id}: {w:?}\n{}", w.hex());
                }
                Err(e) => {
                    let _ = writeln!(r, "\nWindow {id}: {e}");
                }
            }
        }
        Ok(r)
    }
}

impl Drop for Scanner {
    fn drop(&mut self) {
        if self.reserved {
            let _ = self.release();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{ColorMode, ScanArea};
    use crate::scsi::sim::Simulator;

    #[test]
    fn scan_against_the_simulator() {
        let mut s = Scanner::open(Box::new(Simulator::default())).unwrap();
        assert_eq!(s.inquiry.product, "FT-S5000");
        let ws = WindowSettings::new(ScanArea::new(30.0, 30.0, 70.0, 56.0), 150.0, ColorMode::Rgb48);
        let cancel = AtomicBool::new(false);
        let mut last = 0.0;
        let img = s.scan(&ws, &mut |p| last = p, &cancel).unwrap();
        let g = ws.geometry();
        assert_eq!((img.width, img.height), (g.pixels_per_line, g.lines));
        assert_eq!(img.channels, 3);
        assert!((last - 1.0).abs() < 1e-6);
        // the orange mask makes the red channel brighter than blue on a colour negative
        let m = img.channel_means();
        assert!(m[0] > m[2], "means {m:?}");
    }

    #[test]
    fn cancel_stops_the_scan() {
        let mut s = Scanner::open(Box::new(Simulator::ready())).unwrap();
        let ws = WindowSettings::new(ScanArea::new(0.0, 0.0, 100.0, 100.0), 300.0, ColorMode::Gray16);
        let cancel = AtomicBool::new(true);
        assert!(matches!(s.scan(&ws, &mut |_| {}, &cancel), Err(Error::Cancelled)));
    }

    #[test]
    fn probe_is_read_only() {
        let mut s = Scanner::open(Box::new(Simulator::ready())).unwrap();
        let report = s.probe().unwrap();
        assert!(report.contains("FT-S5000"));
        assert!(report.contains("Window 0"));
    }
}
