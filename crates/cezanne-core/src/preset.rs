//! Scan presets: everything needed to repeat a scan, saved as JSON.

use crate::error::Result;
use crate::image::pipeline::{Curve, GrayMode};
use crate::image::tiff::BitDepth;
use crate::image::{Adjustments, FilmType};
use crate::protocol::{ColorMode, LightSource};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Preset {
    pub name: String,
    pub mode: ColorMode,
    pub light: LightSource,
    pub dpi: f64,
    pub adjustments: Adjustments,
    pub output_depth: BitDepth,
    /// Set HD and SD from the prescan automatically.
    pub auto_levels: bool,
}

impl Preset {
    pub fn new(name: &str, film: FilmType) -> Preset {
        let mode = match film {
            FilmType::BwNegative => ColorMode::Rgb48,
            _ => ColorMode::Rgb48,
        };
        Preset {
            name: name.to_string(),
            mode,
            light: if film == FilmType::Reflective { LightSource::Reflective } else { LightSource::Transmissive },
            dpi: 2000.0,
            adjustments: Adjustments::for_film(film),
            output_depth: BitDepth::Sixteen,
            auto_levels: true,
        }
    }

    /// The built in presets, based on the workflows Cezanne owners shared.
    pub fn builtin() -> Vec<Preset> {
        let positive = Preset::new("Colour slide", FilmType::Positive);

        let mut neg = Preset::new("Colour negative", FilmType::ColorNegative);
        neg.adjustments.curve = Curve::s_curve(1.0);

        // Corran's method: positive mode, flipped HD and SD, MIN 0 MAX 255, sharpening off,
        // slight S curve.
        let mut corran = Preset::new("B&W negative (flipped HD/SD)", FilmType::BwNegative);
        corran.adjustments.hd = [2.2; 3];
        corran.adjustments.sd = [0.1; 3];
        corran.adjustments.curve = Curve::s_curve(0.8);
        corran.adjustments.gray = GrayMode::Channel(1);

        // Peter's method: 16 bit, sharpening off, levels 0 and 255, invert, then keep the
        // best channel.
        let mut peter = Preset::new("B&W negative (best channel)", FilmType::BwNegative);
        peter.auto_levels = true;
        peter.adjustments.gray = GrayMode::Channel(1);
        peter.adjustments.curve = Curve::linear();

        let mut print = Preset::new("Print (reflective)", FilmType::Reflective);
        print.dpi = 600.0;

        vec![positive, neg, corran, peter, print]
    }

    pub fn dir() -> PathBuf {
        config_dir().join("presets")
    }

    pub fn save(&self) -> Result<PathBuf> {
        let dir = Self::dir();
        std::fs::create_dir_all(&dir)?;
        let safe: String = self.name.chars().map(|c| if c.is_alphanumeric() || c == ' ' { c } else { '_' }).collect();
        let path = dir.join(format!("{}.json", safe.trim()));
        std::fs::write(&path, serde_json::to_vec_pretty(self).expect("preset serialises"))?;
        Ok(path)
    }

    /// Built in presets followed by the user's saved ones.
    pub fn load_all() -> Vec<Preset> {
        let mut v = Self::builtin();
        if let Ok(rd) = std::fs::read_dir(Self::dir()) {
            let mut user: Vec<Preset> = rd
                .filter_map(|e| e.ok())
                .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
                .filter_map(|e| std::fs::read(e.path()).ok())
                .filter_map(|b| serde_json::from_slice(&b).ok())
                .collect();
            user.sort_by(|a, b| a.name.cmp(&b.name));
            v.extend(user);
        }
        v
    }
}

/// Per user configuration directory.
pub fn config_dir() -> PathBuf {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from).unwrap_or_default();
    if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or(home).join("Cezanne Scan")
    } else if cfg!(target_os = "macos") {
        home.join("Library/Application Support/Cezanne Scan")
    } else {
        std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".config")).join("cezanne-scan")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_round_trip_through_json() {
        for p in Preset::builtin() {
            let j = serde_json::to_string(&p).unwrap();
            let back: Preset = serde_json::from_str(&j).unwrap();
            assert_eq!(back, p);
        }
    }

    #[test]
    fn flipped_preset_inverts() {
        let p = Preset::builtin().into_iter().find(|p| p.name.contains("flipped")).unwrap();
        assert!(p.adjustments.hd[0] > p.adjustments.sd[0]);
    }
}
