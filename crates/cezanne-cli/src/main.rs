//! `cezanne-cli`: discover, probe and scan from the command line.

use cezanne_core::connect::Connection;
use cezanne_core::image::{icc, tiff, Adjustments, FilmType};
use cezanne_core::preset::Preset;
use cezanne_core::protocol::{AreaUnit, ColorMode, ScanArea, WindowSettings};
use cezanne_core::scanner::Scanner;
use cezanne_core::scsi;
use clap::{Args, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

#[derive(Parser)]
#[command(name = "cezanne-cli", version, about = "SCREEN Cezanne FT-S5000 command line tool")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Args, Clone)]
struct Conn {
    /// Use the built in simulated scanner.
    #[arg(long, global = true)]
    sim: bool,
    /// Local device address (`/dev/sg3` on Linux, `scsi2:0:5:0` on Windows). Default: first found.
    #[arg(long, global = true)]
    device: Option<String>,
    /// Connect to a cezanne-bridge, `host` or `host:port`.
    #[arg(long, global = true)]
    bridge: Option<String>,
    /// Token configured on the bridge.
    #[arg(long, global = true, default_value = "")]
    token: String,
}

impl Conn {
    fn connection(&self) -> Connection {
        if self.sim {
            Connection::Simulator
        } else if let Some(b) = &self.bridge {
            Connection::Bridge { host: b.clone(), token: self.token.clone() }
        } else if let Some(d) = &self.device {
            Connection::Local(d.clone())
        } else {
            Connection::AutoLocal
        }
    }
}

#[derive(Subcommand)]
enum Cmd {
    /// List SCREEN scanners attached to this computer.
    List,
    /// Read only report: identity, status and the scanner's stored windows. Safe at any time.
    Probe {
        #[command(flatten)]
        conn: Conn,
    },
    /// Show the scanner's current sense data.
    Sense {
        #[command(flatten)]
        conn: Conn,
    },
    /// Scan an area to a TIFF file.
    Scan(ScanArgs),
    /// List the built in and saved presets.
    Presets,
}

#[derive(Args)]
struct ScanArgs {
    #[command(flatten)]
    conn: Conn,
    /// Output TIFF path.
    #[arg(short, long)]
    out: PathBuf,
    /// Area in millimetres from the top left of the bed: x,y,width,height.
    #[arg(long, value_parser = parse_area, default_value = "30,30,70,56")]
    area: ScanArea,
    /// Resolution in dots per inch.
    #[arg(long, default_value_t = 1000.0)]
    dpi: f64,
    #[arg(long, value_enum, default_value_t = Mode::Rgb48)]
    mode: Mode,
    #[arg(long, value_enum, default_value_t = Film::Positive)]
    film: Film,
    /// Start from a preset by name (see `presets`). Command line options still apply on top.
    #[arg(long)]
    preset: Option<String>,
    /// Set HD and SD from the scan itself.
    #[arg(long)]
    auto: bool,
    /// Write the raw linear scanner data with no adjustments.
    #[arg(long)]
    raw: bool,
    /// Save 8 bits per sample instead of 16.
    #[arg(long)]
    eight_bit: bool,
    /// Embed this ICC profile instead of the generated one.
    #[arg(long)]
    icc: Option<PathBuf>,
    /// Unit of the area fields sent to the scanner (hardware calibration aid).
    #[arg(long, value_enum, default_value_t = Unit::TenthMm)]
    area_unit: Unit,
    /// Factor between dpi and the 32 bit resolution fields (hardware calibration aid).
    #[arg(long, default_value_t = 10)]
    res_scale: u32,
}

#[derive(Clone, Copy, ValueEnum)]
enum Mode {
    Gray8,
    Gray16,
    Rgb24,
    Rgb48,
}

#[derive(Clone, Copy, ValueEnum)]
enum Film {
    Positive,
    ColorNegative,
    BwNegative,
    Print,
}

#[derive(Clone, Copy, ValueEnum)]
enum Unit {
    TenthMm,
    Inch1200,
}

fn parse_area(s: &str) -> Result<ScanArea, String> {
    let v: Vec<f64> = s.split(',').map(|p| p.trim().parse::<f64>().map_err(|e| e.to_string())).collect::<Result<_, _>>()?;
    if v.len() != 4 {
        return Err("expected x,y,width,height in mm".into());
    }
    Ok(ScanArea::new(v[0], v[1], v[2], v[3]))
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let cli = Cli::parse();
    if let Err(e) = run(cli) {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn open(conn: &Conn) -> cezanne_core::Result<Scanner> {
    Scanner::open(conn.connection().open()?)
}

fn run(cli: Cli) -> cezanne_core::Result<()> {
    match cli.cmd {
        Cmd::List => {
            let found = scsi::discover()?;
            if found.is_empty() {
                println!("No SCREEN scanner found. Check the SCSI cable, termination and that the scanner was on before the computer started.");
            }
            for d in found {
                println!("{}  {} {} rev {}", d.address, d.vendor, d.product, d.revision);
            }
        }
        Cmd::Probe { conn } => {
            let mut s = open(&conn)?;
            println!("{}", s.probe()?);
        }
        Cmd::Sense { conn } => {
            let mut s = open(&conn)?;
            println!("{}", s.request_sense()?);
        }
        Cmd::Presets => {
            for p in Preset::load_all() {
                println!("{:32} {:?} {} dpi {:?}", p.name, p.adjustments.film, p.dpi, p.mode);
            }
        }
        Cmd::Scan(a) => scan(a)?,
    }
    Ok(())
}

fn scan(a: ScanArgs) -> cezanne_core::Result<()> {
    let film = match a.film {
        Film::Positive => FilmType::Positive,
        Film::ColorNegative => FilmType::ColorNegative,
        Film::BwNegative => FilmType::BwNegative,
        Film::Print => FilmType::Reflective,
    };
    let mut adj = Adjustments::for_film(film);
    let mut auto = a.auto;
    if let Some(name) = &a.preset {
        let p = Preset::load_all()
            .into_iter()
            .find(|p| p.name.eq_ignore_ascii_case(name))
            .ok_or_else(|| cezanne_core::Error::InvalidParameter(format!("no preset called {name}")))?;
        adj = p.adjustments;
        auto |= p.auto_levels;
    }
    let mode = match a.mode {
        Mode::Gray8 => ColorMode::Gray8,
        Mode::Gray16 => ColorMode::Gray16,
        Mode::Rgb24 => ColorMode::Rgb24,
        Mode::Rgb48 => ColorMode::Rgb48,
    };
    let mut ws = WindowSettings::new(a.area, a.dpi, mode);
    ws.original = film.original();
    ws.area_unit = match a.area_unit {
        Unit::TenthMm => AreaUnit::TenthMm,
        Unit::Inch1200 => AreaUnit::Inch1200,
    };
    ws.resolution_scale = a.res_scale;
    let g = ws.geometry();
    eprintln!(
        "Scanning {:.1} x {:.1} mm at {} dpi: {} x {} pixels, {:.1} MB",
        a.area.width_mm,
        a.area.height_mm,
        a.dpi,
        g.pixels_per_line,
        g.lines,
        g.total_bytes() as f64 / 1e6
    );

    let mut s = open(&a.conn)?;
    eprintln!("{}", s.describe());
    let cancel = AtomicBool::new(false);
    let mut last = -1i32;
    let raw = s.scan(
        &ws,
        &mut |p| {
            let pct = (p * 100.0) as i32;
            if pct / 5 != last / 5 {
                last = pct;
                eprint!("\r{pct:3}%");
            }
        },
        &cancel,
    )?;
    eprintln!();

    let out = if a.raw {
        // straight linear copy of the scanner data
        cezanne_core::image::Processed {
            width: raw.width,
            height: raw.height,
            channels: raw.channels,
            x_dpi: raw.x_dpi,
            y_dpi: raw.y_dpi,
            data: raw.data.clone(),
        }
    } else {
        if auto {
            adj.auto_levels(&raw, 0.002);
            eprintln!("Auto levels: HD {:.2?} SD {:.2?}", adj.hd, adj.sd);
        }
        adj.apply(&raw)
    };
    let profile = match &a.icc {
        Some(p) => std::fs::read(p)?,
        None if a.raw => {
            if out.channels == 3 {
                icc::rgb_profile(1.0)
            } else {
                icc::gray_profile(1.0)
            }
        }
        None => {
            if out.channels == 3 {
                icc::rgb_profile(2.2)
            } else {
                icc::gray_profile(2.2)
            }
        }
    };
    let depth = if a.eight_bit { tiff::BitDepth::Eight } else { tiff::BitDepth::Sixteen };
    tiff::write_tiff(&a.out, &out, depth, Some(&profile))?;
    eprintln!("Saved {} ({} x {})", a.out.display(), out.width, out.height);
    Ok(())
}
