//! SET WINDOW descriptors.
//!
//! The FT-S5000 uses 176 byte window descriptors. The layout comes from two sources that
//! agree with each other: the scanner firmware's SET WINDOW validator and scan engine, and
//! SCREEN's own host driver ("FT-S5000 Driver" 2.1.3, function `SetJobToDevice`), which
//! shows exactly which bytes ColorGeniusEX fills and with what. See `docs/protocol.md`.
//!
//! Units, confirmed from the driver: scan area offsets are 0.1 mm, the 32 bit resolution
//! fields are dpi times 10. The scanner works out the scanned extent from the pixel
//! count and the resolution, so the SCSI-2 width and length fields stay zero, as in the
//! original driver.

use super::{be16, be32};
use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};

pub const DESCRIPTOR_LEN: usize = 176;
pub const HEADER_LEN: usize = 8;

/// Descriptor offsets.
pub mod off {
    pub const WINDOW_ID: usize = 0;
    pub const UPPER_LEFT_X: usize = 6;
    pub const UPPER_LEFT_Y: usize = 10;
    pub const WIDTH: usize = 14;
    pub const LENGTH: usize = 18;
    /// Three constant vendor bytes the driver always sends: 2, 100, 0x20.
    pub const VENDOR_CONST: usize = 41;
    /// 1 for a prescan, 2 for a normal scan (6, 7, 8 are line art and calibration modes).
    pub const SCAN_MODE: usize = 45;
    pub const PIXELS_PER_LINE: usize = 46;
    pub const LINES: usize = 48;
    pub const COMPOSITION: usize = 50;
    pub const BITS: usize = 51;
    pub const X_RESOLUTION: usize = 54;
    pub const Y_RESOLUTION: usize = 58;
    /// Four lookup table selector bytes, set per original type.
    pub const LUT_ID: usize = 62;
    /// 2 for reflective originals, 0 for film.
    pub const ORIGINAL_CLASS: usize = 142;
    /// 1 for reflective originals (reflective lamp), 0 for film (transmission lamp).
    pub const LIGHT_SOURCE: usize = 144;
}

/// Image composition and depth.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ColorMode {
    Gray8,
    Gray16,
    Rgb24,
    Rgb48,
}

impl ColorMode {
    /// SCSI-2 image composition code.
    pub fn composition(self) -> u8 {
        match self {
            ColorMode::Gray8 | ColorMode::Gray16 => 2,
            ColorMode::Rgb24 | ColorMode::Rgb48 => 5,
        }
    }

    pub fn bits_per_pixel(self) -> u8 {
        match self {
            ColorMode::Gray8 => 8,
            ColorMode::Gray16 => 16,
            ColorMode::Rgb24 => 24,
            ColorMode::Rgb48 => 48,
        }
    }

    pub fn channels(self) -> usize {
        match self {
            ColorMode::Gray8 | ColorMode::Gray16 => 1,
            ColorMode::Rgb24 | ColorMode::Rgb48 => 3,
        }
    }

    pub fn bytes_per_sample(self) -> usize {
        match self {
            ColorMode::Gray8 | ColorMode::Rgb24 => 1,
            ColorMode::Gray16 | ColorMode::Rgb48 => 2,
        }
    }

    /// Bytes per pixel as the firmware computes it (1, 2, 3 or 6).
    pub fn bytes_per_pixel(self) -> usize {
        self.channels() * self.bytes_per_sample()
    }

    pub fn is_gray(self) -> bool {
        self.channels() == 1
    }

    pub fn from_codes(composition: u8, bits: u8) -> Option<ColorMode> {
        match (composition, bits) {
            (2, 8) => Some(ColorMode::Gray8),
            (2, 16) => Some(ColorMode::Gray16),
            (5, 24) => Some(ColorMode::Rgb24),
            (5, 48) => Some(ColorMode::Rgb48),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ColorMode::Gray8 => "Grey 8 bit",
            ColorMode::Gray16 => "Grey 16 bit",
            ColorMode::Rgb24 => "Colour 24 bit",
            ColorMode::Rgb48 => "Colour 48 bit",
        }
    }

    pub const ALL: [ColorMode; 4] = [ColorMode::Gray8, ColorMode::Gray16, ColorMode::Rgb24, ColorMode::Rgb48];
}

/// Which lamp lights the original.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LightSource {
    /// Film and other transparent originals (the usual Cezanne use).
    Transmissive,
    /// Prints and other opaque originals.
    Reflective,
}

/// The original types of the SCREEN driver (its "OriType" list) that produce continuous
/// tone images. Line art and colour separation types are not supported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OriginalType {
    PositiveFilm,
    NegativeFilm,
    Photograph,
    PrintedMaterial,
    Watercolor,
}

impl OriginalType {
    pub fn is_reflective(self) -> bool {
        !matches!(self, OriginalType::PositiveFilm | OriginalType::NegativeFilm)
    }

    pub fn light(self) -> LightSource {
        if self.is_reflective() {
            LightSource::Reflective
        } else {
            LightSource::Transmissive
        }
    }

    /// The driver's lookup table selector string (STR# 2000) for this type and output.
    pub fn lut_code(self, gray: bool) -> &'static str {
        match (self, gray) {
            (OriginalType::PositiveFilm, false) => "11003",
            (OriginalType::PositiveFilm, true) => "11006",
            (OriginalType::NegativeFilm, _) => "11005",
            _ => "22004",
        }
    }

    /// The four selector bytes, packed the way the driver's `SetSCSIWindowBasicLUTID` does.
    pub fn lut_bytes(self, gray: bool) -> [u8; 4] {
        let d: Vec<u8> = self.lut_code(gray).bytes().map(|c| c - b'0').collect();
        [d[2], d[0], d[4], (d[3] << 4) | d[1]]
    }

    pub fn label(self) -> &'static str {
        match self {
            OriginalType::PositiveFilm => "Positive film",
            OriginalType::NegativeFilm => "Negative film",
            OriginalType::Photograph => "Photograph",
            OriginalType::PrintedMaterial => "Printed material",
            OriginalType::Watercolor => "Watercolor",
        }
    }
}

/// Unit of the scan area offsets (offsets 6 to 13).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AreaUnit {
    /// 0.1 mm, confirmed from the SCREEN driver.
    TenthMm,
    /// 1/1200 inch, the SCSI-2 default. Kept only as a diagnostic option.
    Inch1200,
}

impl AreaUnit {
    pub fn units_per_mm(self) -> f64 {
        match self {
            AreaUnit::TenthMm => 10.0,
            AreaUnit::Inch1200 => 1200.0 / 25.4,
        }
    }
}

/// Firmware validation limits for one model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelLimits {
    pub max_x: u32,
    pub max_y: u32,
    pub max_pixels_per_line: u16,
    pub resolution_min: u32,
    pub resolution_max: u32,
}

impl ModelLimits {
    pub const FT_S5000: ModelLimits = ModelLimits {
        max_x: 3420,
        max_y: 5310,
        max_pixels_per_line: 12000,
        resolution_min: 200,
        resolution_max: 120_000,
    };

    pub const FT_S3000: ModelLimits = ModelLimits {
        max_x: 3420,
        max_y: 5310,
        max_pixels_per_line: 12000,
        resolution_min: 200,
        resolution_max: 66_000,
    };

    pub fn for_product(product: &str) -> ModelLimits {
        if product.starts_with("FT-S3") {
            Self::FT_S3000
        } else {
            Self::FT_S5000
        }
    }

    /// Bed size in mm for an area unit.
    pub fn bed_mm(&self, unit: AreaUnit) -> (f64, f64) {
        (self.max_x as f64 / unit.units_per_mm(), self.max_y as f64 / unit.units_per_mm())
    }
}

/// A rectangle on the scanner bed in millimetres, origin top left.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ScanArea {
    pub x_mm: f64,
    pub y_mm: f64,
    pub width_mm: f64,
    pub height_mm: f64,
}

impl ScanArea {
    pub fn new(x_mm: f64, y_mm: f64, width_mm: f64, height_mm: f64) -> Self {
        ScanArea { x_mm, y_mm, width_mm, height_mm }
    }

    pub fn whole_bed(limits: &ModelLimits, unit: AreaUnit) -> Self {
        let (w, h) = limits.bed_mm(unit);
        ScanArea::new(0.0, 0.0, w, h)
    }
}

/// Everything needed to build one window.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WindowSettings {
    pub window_id: u8,
    pub area: ScanArea,
    pub x_dpi: f64,
    pub y_dpi: f64,
    pub mode: ColorMode,
    pub original: OriginalType,
    /// Prescan (fast, lower quality) rather than a final scan.
    pub preview: bool,
    pub area_unit: AreaUnit,
    /// The 32 bit resolution fields hold dpi times this factor (10, confirmed).
    pub resolution_scale: u32,
}

impl WindowSettings {
    pub fn new(area: ScanArea, dpi: f64, mode: ColorMode) -> Self {
        WindowSettings {
            window_id: 1,
            area,
            x_dpi: dpi,
            y_dpi: dpi,
            mode,
            original: OriginalType::PositiveFilm,
            preview: false,
            area_unit: AreaUnit::TenthMm,
            resolution_scale: 10,
        }
    }

    pub fn light(&self) -> LightSource {
        self.original.light()
    }

    pub fn geometry(&self) -> Geometry {
        // round down (with a little tolerance) so the scan never runs past the requested area
        let px = |mm: f64, dpi: f64| ((mm / 25.4 * dpi + 1e-6).floor() as i64).max(1) as u32;
        Geometry {
            pixels_per_line: px(self.area.width_mm, self.x_dpi),
            lines: px(self.area.height_mm, self.y_dpi),
            mode: self.mode,
        }
    }
}

/// Image size the scanner will deliver.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Geometry {
    pub pixels_per_line: u32,
    pub lines: u32,
    pub mode: ColorMode,
}

impl Geometry {
    pub fn bytes_per_line(&self) -> usize {
        self.pixels_per_line as usize * self.mode.bytes_per_pixel()
    }

    pub fn total_bytes(&self) -> usize {
        self.bytes_per_line() * self.lines as usize
    }
}

/// One raw 176 byte window descriptor.
#[derive(Clone, PartialEq, Eq)]
pub struct WindowDescriptor(pub [u8; DESCRIPTOR_LEN]);

impl std::fmt::Debug for WindowDescriptor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WindowDescriptor")
            .field("id", &self.window_id())
            .field("ulx", &self.upper_left_x())
            .field("uly", &self.upper_left_y())
            .field("pixels_per_line", &self.pixels_per_line())
            .field("lines", &self.lines())
            .field("composition", &self.0[off::COMPOSITION])
            .field("bits", &self.0[off::BITS])
            .field("x_res", &self.x_resolution())
            .field("y_res", &self.y_resolution())
            .field("mode", &self.0[off::SCAN_MODE])
            .field("lut", &&self.0[off::LUT_ID..off::LUT_ID + 4])
            .field("light", &self.0[off::LIGHT_SOURCE])
            .finish()
    }
}

/// Convert a pixel count at a resolution field value to the extent in 0.1 mm.
fn extent_tenth_mm(pixels: u32, res_field: u32) -> u32 {
    if res_field == 0 {
        return u32::MAX;
    }
    // pixels / (res_field / 10) inches, 254 tenths of a mm per inch
    ((pixels as u64 * 2540 + res_field as u64 - 1) / res_field as u64) as u32
}

impl WindowDescriptor {
    pub fn zeroed() -> Self {
        WindowDescriptor([0; DESCRIPTOR_LEN])
    }

    pub fn from_bytes(b: &[u8]) -> Option<Self> {
        let mut d = [0u8; DESCRIPTOR_LEN];
        if b.len() < DESCRIPTOR_LEN {
            return None;
        }
        d.copy_from_slice(&b[..DESCRIPTOR_LEN]);
        Some(WindowDescriptor(d))
    }

    /// Build a descriptor from settings, filling the same bytes as SCREEN's driver.
    pub fn build(s: &WindowSettings) -> Result<(WindowDescriptor, Geometry)> {
        if s.x_dpi <= 0.0 || s.y_dpi <= 0.0 {
            return Err(Error::InvalidParameter("resolution must be positive".into()));
        }
        let g = s.geometry();
        let mut d = WindowDescriptor::zeroed();
        let u = s.area_unit.units_per_mm();
        let to_units = |mm: f64| (mm * u).round().max(0.0) as u32;

        d.0[off::WINDOW_ID] = s.window_id;
        d.put32(off::UPPER_LEFT_X, to_units(s.area.x_mm));
        d.put32(off::UPPER_LEFT_Y, to_units(s.area.y_mm));
        d.0[off::VENDOR_CONST..off::VENDOR_CONST + 3].copy_from_slice(&[2, 100, 0x20]);
        d.0[off::SCAN_MODE] = if s.preview { 1 } else { 2 };
        d.put16(off::PIXELS_PER_LINE, u16::try_from(g.pixels_per_line).map_err(|_| too_big("pixels per line"))?);
        d.put16(off::LINES, u16::try_from(g.lines).map_err(|_| too_big("line count"))?);
        d.0[off::COMPOSITION] = s.mode.composition();
        d.0[off::BITS] = s.mode.bits_per_pixel();
        d.put32(off::X_RESOLUTION, (s.x_dpi * s.resolution_scale as f64).round() as u32);
        d.put32(off::Y_RESOLUTION, (s.y_dpi * s.resolution_scale as f64).round() as u32);
        d.0[off::LUT_ID..off::LUT_ID + 4].copy_from_slice(&s.original.lut_bytes(s.mode.is_gray()));
        let reflective = s.original.is_reflective();
        d.0[off::ORIGINAL_CLASS] = if reflective { 2 } else { 0 };
        d.0[off::LIGHT_SOURCE] = reflective as u8;
        Ok((d, g))
    }

    /// The same checks the firmware runs before accepting a window, so errors are caught
    /// with a clear message instead of a bare ILLEGAL REQUEST.
    pub fn validate(&self, limits: &ModelLimits) -> Result<()> {
        let bad = |m: String| Err(Error::InvalidParameter(m));
        let xr = self.x_resolution();
        let yr = self.y_resolution();
        let rr = limits.resolution_min..=limits.resolution_max;
        if !rr.contains(&xr) {
            return bad(format!("X resolution field {xr} outside {}..={}", limits.resolution_min, limits.resolution_max));
        }
        if !rr.contains(&yr) {
            return bad(format!("Y resolution field {yr} outside {}..={}", limits.resolution_min, limits.resolution_max));
        }
        if self.pixels_per_line() > limits.max_pixels_per_line {
            return bad(format!("{} pixels per line is above the limit of {}", self.pixels_per_line(), limits.max_pixels_per_line));
        }
        let x_end = self.upper_left_x().saturating_add(extent_tenth_mm(self.pixels_per_line() as u32, xr));
        if x_end > limits.max_x {
            return bad(format!("area runs past the bed width ({x_end} > {})", limits.max_x));
        }
        let y_end = self.upper_left_y().saturating_add(extent_tenth_mm(self.lines() as u32, yr));
        if y_end > limits.max_y {
            return bad(format!("area runs past the bed length ({y_end} > {})", limits.max_y));
        }
        if self.color_mode().is_none() {
            return bad(format!("unsupported composition {} with {} bits", self.0[off::COMPOSITION], self.0[off::BITS]));
        }
        Ok(())
    }

    pub fn geometry(&self) -> Option<Geometry> {
        Some(Geometry {
            pixels_per_line: self.pixels_per_line() as u32,
            lines: self.lines() as u32,
            mode: self.color_mode()?,
        })
    }

    /// Scanned extent in 0.1 mm (width, length), as the scanner works it out.
    pub fn extent(&self) -> (u32, u32) {
        (
            extent_tenth_mm(self.pixels_per_line() as u32, self.x_resolution()),
            extent_tenth_mm(self.lines() as u32, self.y_resolution()),
        )
    }

    pub fn window_id(&self) -> u8 {
        self.0[off::WINDOW_ID]
    }
    pub fn upper_left_x(&self) -> u32 {
        be32(&self.0[off::UPPER_LEFT_X..])
    }
    pub fn upper_left_y(&self) -> u32 {
        be32(&self.0[off::UPPER_LEFT_Y..])
    }
    pub fn width(&self) -> u32 {
        be32(&self.0[off::WIDTH..])
    }
    pub fn length(&self) -> u32 {
        be32(&self.0[off::LENGTH..])
    }
    pub fn pixels_per_line(&self) -> u16 {
        be16(&self.0[off::PIXELS_PER_LINE..])
    }
    pub fn lines(&self) -> u16 {
        be16(&self.0[off::LINES..])
    }
    pub fn x_resolution(&self) -> u32 {
        be32(&self.0[off::X_RESOLUTION..])
    }
    pub fn y_resolution(&self) -> u32 {
        be32(&self.0[off::Y_RESOLUTION..])
    }
    pub fn light_source(&self) -> u8 {
        self.0[off::LIGHT_SOURCE]
    }
    pub fn color_mode(&self) -> Option<ColorMode> {
        ColorMode::from_codes(self.0[off::COMPOSITION], self.0[off::BITS])
    }

    fn put16(&mut self, at: usize, v: u16) {
        self.0[at..at + 2].copy_from_slice(&v.to_be_bytes());
    }
    fn put32(&mut self, at: usize, v: u32) {
        self.0[at..at + 4].copy_from_slice(&v.to_be_bytes());
    }

    /// Hex dump, 16 bytes per row, for logs and the `probe` command.
    pub fn hex(&self) -> String {
        self.0
            .chunks(16)
            .enumerate()
            .map(|(i, c)| {
                let h: Vec<String> = c.iter().map(|b| format!("{b:02x}")).collect();
                format!("{:3}: {}", i * 16, h.join(" "))
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn too_big(what: &str) -> Error {
    Error::InvalidParameter(format!("{what} does not fit in 16 bits, lower the resolution or the area"))
}

/// Build a SET WINDOW parameter list: 8 byte header then the descriptors. Like SCREEN's
/// driver, bytes 0 and 1 hold the window data length and bytes 6 and 7 the descriptor length.
pub fn parameter_list(windows: &[WindowDescriptor]) -> Vec<u8> {
    let mut v = vec![0u8; HEADER_LEN];
    let data_len = (windows.len() * DESCRIPTOR_LEN + 6) as u16;
    v[0..2].copy_from_slice(&data_len.to_be_bytes());
    v[6..8].copy_from_slice(&(DESCRIPTOR_LEN as u16).to_be_bytes());
    for w in windows {
        v.extend_from_slice(&w.0);
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> WindowSettings {
        WindowSettings::new(ScanArea::new(10.0, 20.0, 60.0, 90.0), 2000.0, ColorMode::Rgb48)
    }

    #[test]
    fn geometry_matches_area_and_dpi() {
        let g = settings().geometry();
        assert_eq!(g.pixels_per_line, (60.0f64 / 25.4 * 2000.0).floor() as u32);
        assert_eq!(g.lines, (90.0f64 / 25.4 * 2000.0).floor() as u32);
        assert_eq!(g.bytes_per_line(), g.pixels_per_line as usize * 6);
    }

    #[test]
    fn descriptor_matches_the_screen_driver() {
        let (d, g) = WindowDescriptor::build(&settings()).unwrap();
        assert_eq!(d.window_id(), 1);
        assert_eq!(d.upper_left_x(), 100);
        assert_eq!(d.upper_left_y(), 200);
        // the driver leaves the SCSI-2 width and length at zero
        assert_eq!((d.width(), d.length()), (0, 0));
        assert_eq!(&d.0[41..44], &[2, 100, 0x20]);
        assert_eq!(d.0[off::SCAN_MODE], 2);
        assert_eq!(d.pixels_per_line() as u32, g.pixels_per_line);
        assert_eq!(d.color_mode(), Some(ColorMode::Rgb48));
        assert_eq!(d.x_resolution(), 20000);
        assert_eq!(&d.0[62..66], &[0, 1, 3, 0x01]);
        assert_eq!((d.0[142], d.0[144]), (0, 0));
        assert!(d.validate(&ModelLimits::FT_S5000).is_ok());
        // the extent the scanner derives is the requested area
        let (w, h) = d.extent();
        assert!((w as i32 - 600).abs() <= 1 && (h as i32 - 900).abs() <= 1, "{w} {h}");
        let back = WindowDescriptor::from_bytes(&d.0).unwrap();
        assert_eq!(back.geometry(), Some(g));
    }

    #[test]
    fn whole_bed_fits() {
        for dpi in [50.0, 75.0, 100.0, 300.0] {
            let s = WindowSettings::new(ScanArea::whole_bed(&ModelLimits::FT_S5000, AreaUnit::TenthMm), dpi, ColorMode::Rgb48);
            let (d, _) = WindowDescriptor::build(&s).unwrap();
            assert!(d.validate(&ModelLimits::FT_S5000).is_ok(), "{dpi} dpi");
        }
    }

    #[test]
    fn lut_codes_per_original() {
        assert_eq!(OriginalType::PositiveFilm.lut_bytes(true), [0, 1, 6, 0x01]);
        assert_eq!(OriginalType::NegativeFilm.lut_bytes(false), [0, 1, 5, 0x01]);
        assert_eq!(OriginalType::Photograph.lut_bytes(false), [0, 2, 4, 0x02]);
    }

    #[test]
    fn reflective_originals_select_the_reflective_lamp() {
        let mut s = settings();
        s.original = OriginalType::Photograph;
        s.preview = true;
        let (d, _) = WindowDescriptor::build(&s).unwrap();
        assert_eq!((d.0[142], d.0[144], d.0[45]), (2, 1, 1));
    }

    #[test]
    fn validation_catches_area_past_bed() {
        let mut s = settings();
        s.area = ScanArea::new(300.0, 0.0, 60.0, 10.0);
        s.x_dpi = 300.0;
        s.y_dpi = 300.0;
        let (d, _) = WindowDescriptor::build(&s).unwrap();
        assert!(d.validate(&ModelLimits::FT_S5000).is_err());
    }

    #[test]
    fn validation_catches_resolution_limits() {
        let mut s = settings();
        s.x_dpi = 13000.0;
        s.area.width_mm = 1.0;
        let (d, _) = WindowDescriptor::build(&s).unwrap();
        assert!(d.validate(&ModelLimits::FT_S5000).is_err());
    }

    #[test]
    fn parameter_list_header() {
        let (d, _) = WindowDescriptor::build(&settings()).unwrap();
        let p = parameter_list(&[d]);
        assert_eq!(p.len(), 184);
        assert_eq!(&p[0..2], &[0, 182]);
        assert_eq!(&p[6..8], &[0, 176]);
    }
}
