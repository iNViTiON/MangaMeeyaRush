//! INI-based configuration for mmce, compatible with legacy MangaMeeyaCE.ini.
//!
//! The legacy file is UTF-16 LE with BOM and `;` comments. We accept both UTF-16
//! and UTF-8 on read, and we write UTF-16 LE with BOM on save for round-trip.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use encoding_rs::{UTF_16LE, UTF_8};
use serde::{Deserialize, Serialize};

pub mod ini;

pub use ini::Ini;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("decode: {0}")]
    Decode(String),
}

/// High-level typed settings pulled from the INI. We load only what we actually
/// honour; everything else is preserved verbatim on save via `extra`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    pub general: General,
    pub view: ViewMode,
    pub scale: ScaleMode,
    pub cache: Cache,
    pub scroll: Scroll,
    /// Sections we don't interpret — stashed so re-saving doesn't destroy them.
    #[serde(skip)]
    pub extra: Ini,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct General {
    pub current_folder: Option<PathBuf>,
    pub bg_color: u32,
    pub fullscreen: bool,
    pub confirm_delete: bool,
    pub sub_folder_load: u8,
}

impl Default for General {
    fn default() -> Self {
        Self {
            current_folder: None,
            bg_color: 0,
            // Default to fullscreen. The user can drop out with F11 / Esc /
            // Alt+Enter and that choice is preserved in the INI on exit, so
            // subsequent sessions honour the last state — but a fresh
            // install lands fullscreen.
            fullscreen: true,
            confirm_delete: true,
            sub_folder_load: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ViewMode {
    pub page_mode: PageMode,
    pub bind_dir: BindDir,
    pub sort: SortMode,
    pub auto_page_mode: bool,
}

impl Default for ViewMode {
    fn default() -> Self {
        Self {
            page_mode: PageMode::Auto,
            bind_dir: BindDir::RightToLeft,
            sort: SortMode::NameNatural,
            auto_page_mode: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PageMode {
    Single,
    Spread,
    /// Spread by default, but falls back to single when either page of the
    /// current pair is landscape (wider than tall). Matches the legacy
    /// `AutoPageMode` behaviour.
    Auto,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BindDir {
    LeftToRight,
    RightToLeft,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SortMode {
    NameNatural,
    NameLex,
    Date,
    Size,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ScaleMode {
    pub mode: FitMode,
    pub no_zoom_in: bool,
    pub optional_scale: f32,
}

impl Default for ScaleMode {
    fn default() -> Self {
        Self {
            mode: FitMode::Fit,
            // Fit should actually fit — upscale small images. The user can
            // opt back in via INI (NoZoomIn=1).
            no_zoom_in: false,
            optional_scale: 1.0,
        }
    }
}

/// Matches the legacy `ScaleMode.Mode` meanings where possible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FitMode {
    /// 100%
    Original,
    /// Fit whole page in window.
    Fit,
    /// Fit width of page.
    FitWidth,
    /// Fit height of page.
    FitHeight,
    /// Optional (user-set) fixed scale.
    Custom,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Cache {
    pub picture_cache_size: u32,
    pub file_cache_size: u32,
    pub preload: bool,
    pub load_scaled: bool,
}

impl Default for Cache {
    fn default() -> Self {
        Self {
            picture_cache_size: 64,
            file_cache_size: 64,
            preload: true,
            load_scaled: true,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Scroll {
    pub smooth: bool,
    pub speed: u32,
    pub horz_distance: u32,
    pub vert_distance: u32,
}

impl Default for Scroll {
    fn default() -> Self {
        Self {
            smooth: true,
            speed: 50,
            horz_distance: 100,
            vert_distance: 100,
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            general: General::default(),
            view: ViewMode::default(),
            scale: ScaleMode::default(),
            cache: Cache::default(),
            scroll: Scroll::default(),
            extra: Ini::default(),
        }
    }
}

impl Settings {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let bytes = fs::read(path)?;
        let text = decode_any(&bytes)?;
        let ini = Ini::parse(&text);
        Ok(Self::from_ini(ini))
    }

    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        let ini = self.to_ini();
        let text = ini.to_string();
        // UTF-16 LE with BOM.
        let (encoded, _, _) = UTF_16LE.encode(&text);
        let mut out = Vec::with_capacity(2 + encoded.len());
        out.extend_from_slice(&[0xFF, 0xFE]);
        out.extend_from_slice(&encoded);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, out)?;
        Ok(())
    }

    pub fn from_ini(mut ini: Ini) -> Self {
        let mut s = Settings::default();

        if let Some(sec) = ini.take_section("General") {
            s.general.current_folder = sec.get("CurrentFolder").map(PathBuf::from);
            s.general.bg_color = sec.get_parse("BGColor").unwrap_or(0);
            s.general.fullscreen = sec.get_bool("FullScreen").unwrap_or(false);
            s.general.confirm_delete = sec.get_bool("ConfirmDelete").unwrap_or(true);
            s.general.sub_folder_load = sec.get_parse("SubFolderLoad").unwrap_or(0);
        }
        if let Some(sec) = ini.take_section("ViewMode") {
            // AutoPageMode=1 (default) promotes any PageMode to Auto, which
            // collapses to Single when a landscape page appears.
            let auto = sec.get_bool("AutoPageMode").unwrap_or(true);
            s.view.page_mode = match (sec.get_parse::<i32>("PageMode").unwrap_or(1), auto) {
                (0, _) => PageMode::Single,
                (_, true) => PageMode::Auto,
                (_, false) => PageMode::Spread,
            };
            s.view.bind_dir = match sec.get_parse::<i32>("BindDir").unwrap_or(1) {
                0 => BindDir::LeftToRight,
                _ => BindDir::RightToLeft,
            };
            s.view.sort = match sec.get_parse::<i32>("Sort").unwrap_or(11) {
                0..=9 => SortMode::NameLex,
                10..=19 => SortMode::NameNatural,
                20..=29 => SortMode::Date,
                30..=39 => SortMode::Size,
                _ => SortMode::NameNatural,
            };
            s.view.auto_page_mode = auto;
        }
        if let Some(sec) = ini.take_section("ScaleMode") {
            s.scale.mode = match sec.get_parse::<i32>("Mode").unwrap_or(5) {
                0 => FitMode::Original,
                1 | 2 => FitMode::Fit,
                3 => FitMode::FitWidth,
                4 => FitMode::FitHeight,
                _ => FitMode::Fit,
            };
            s.scale.no_zoom_in = sec.get_bool("NoZoomIn").unwrap_or(true);
            s.scale.optional_scale = sec.get_parse("OptionalScale").unwrap_or(1.0);
        }
        if let Some(sec) = ini.take_section("Cache") {
            s.cache.picture_cache_size = sec.get_parse("PictureCacheSize").unwrap_or(64);
            s.cache.file_cache_size = sec.get_parse("FileCacheSize").unwrap_or(64);
            s.cache.preload = sec.get_bool("PreLoad").unwrap_or(true);
            s.cache.load_scaled = sec.get_bool("LoadScaled").unwrap_or(true);
        }
        if let Some(sec) = ini.take_section("Scroll") {
            s.scroll.smooth = sec.get_bool("SmoothScroll").unwrap_or(true);
            s.scroll.speed = sec.get_parse("SmoothScrollSpeed").unwrap_or(50);
            s.scroll.horz_distance = sec.get_parse("HorzDistance").unwrap_or(100);
            s.scroll.vert_distance = sec.get_parse("VertDistance").unwrap_or(100);
        }
        s.extra = ini;
        s
    }

    pub fn to_ini(&self) -> Ini {
        let mut ini = self.extra.clone();
        let gen = ini.section_mut("General");
        if let Some(p) = &self.general.current_folder {
            gen.set("CurrentFolder", p.display().to_string());
        }
        gen.set("BGColor", self.general.bg_color.to_string());
        gen.set("FullScreen", bool_i(self.general.fullscreen));
        gen.set("ConfirmDelete", bool_i(self.general.confirm_delete));
        gen.set("SubFolderLoad", self.general.sub_folder_load.to_string());

        let vm = ini.section_mut("ViewMode");
        vm.set(
            "PageMode",
            match self.view.page_mode {
                PageMode::Single => "0",
                PageMode::Spread | PageMode::Auto => "1",
            },
        );
        // AutoPageMode tells the legacy app to downshift to single on
        // landscape pages — we only emit 1 when we're in Auto.
        vm.set(
            "AutoPageMode",
            if self.view.page_mode == PageMode::Auto { "1" } else { "0" },
        );
        vm.set(
            "BindDir",
            match self.view.bind_dir {
                BindDir::LeftToRight => "0",
                BindDir::RightToLeft => "1",
            },
        );
        vm.set(
            "Sort",
            match self.view.sort {
                SortMode::NameLex => "0",
                SortMode::NameNatural => "11",
                SortMode::Date => "20",
                SortMode::Size => "30",
            },
        );

        let sm = ini.section_mut("ScaleMode");
        sm.set(
            "Mode",
            match self.scale.mode {
                FitMode::Original => "0",
                FitMode::Fit => "2",
                FitMode::FitWidth => "3",
                FitMode::FitHeight => "4",
                FitMode::Custom => "5",
            },
        );
        sm.set("NoZoomIn", bool_i(self.scale.no_zoom_in));
        sm.set("OptionalScale", format!("{}", self.scale.optional_scale));

        let c = ini.section_mut("Cache");
        c.set("PictureCacheSize", self.cache.picture_cache_size.to_string());
        c.set("FileCacheSize", self.cache.file_cache_size.to_string());
        c.set("PreLoad", bool_i(self.cache.preload));
        c.set("LoadScaled", bool_i(self.cache.load_scaled));

        let sc = ini.section_mut("Scroll");
        sc.set("SmoothScroll", bool_i(self.scroll.smooth));
        sc.set("SmoothScrollSpeed", self.scroll.speed.to_string());
        sc.set("HorzDistance", self.scroll.horz_distance.to_string());
        sc.set("VertDistance", self.scroll.vert_distance.to_string());
        ini
    }
}

fn bool_i(b: bool) -> &'static str {
    if b { "1" } else { "0" }
}

fn decode_any(bytes: &[u8]) -> Result<String, ConfigError> {
    if bytes.starts_with(&[0xFF, 0xFE]) {
        let (cow, _, had_errors) = UTF_16LE.decode(&bytes[2..]);
        if had_errors {
            return Err(ConfigError::Decode("invalid UTF-16 LE".into()));
        }
        Ok(cow.into_owned())
    } else if bytes.starts_with(&[0xFE, 0xFF]) {
        use encoding_rs::UTF_16BE;
        let (cow, _, had_errors) = UTF_16BE.decode(&bytes[2..]);
        if had_errors {
            return Err(ConfigError::Decode("invalid UTF-16 BE".into()));
        }
        Ok(cow.into_owned())
    } else {
        let (cow, _, _) = UTF_8.decode(bytes);
        Ok(cow.into_owned())
    }
}

// Re-export for test convenience.
pub fn settings_from_string(text: &str) -> Settings {
    Settings::from_ini(Ini::parse(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_roundtrip() {
        let s = Settings::default();
        let ini = s.to_ini();
        let back = Settings::from_ini(ini);
        assert_eq!(back.view.page_mode, PageMode::Auto);
        assert_eq!(back.view.bind_dir, BindDir::RightToLeft);
    }

    #[test]
    fn parses_legacy_viewmode_single() {
        let text = "[ViewMode]\nPageMode=0\nBindDir=0\nSort=20\nAutoPageMode=0\n";
        let s = settings_from_string(text);
        assert_eq!(s.view.page_mode, PageMode::Single);
        assert_eq!(s.view.bind_dir, BindDir::LeftToRight);
        assert_eq!(s.view.sort, SortMode::Date);
        assert!(!s.view.auto_page_mode);
    }

    #[test]
    fn parses_legacy_viewmode_auto() {
        let text = "[ViewMode]\nPageMode=1\nAutoPageMode=1\n";
        assert_eq!(settings_from_string(text).view.page_mode, PageMode::Auto);
    }

    #[test]
    fn parses_legacy_viewmode_spread_without_auto() {
        let text = "[ViewMode]\nPageMode=1\nAutoPageMode=0\n";
        assert_eq!(settings_from_string(text).view.page_mode, PageMode::Spread);
    }

    #[test]
    fn preserves_unknown_sections() {
        let text = "[Weird]\nKey=hello\n[General]\nBGColor=42\n";
        let s = settings_from_string(text);
        let ini = s.to_ini();
        let serialized = ini.to_string();
        assert!(serialized.contains("[Weird]"));
        assert!(serialized.contains("Key=hello"));
    }
}

// Avoid unused warning for BTreeMap import when tests are off
#[allow(dead_code)]
fn _force_use() -> BTreeMap<(), ()> {
    BTreeMap::new()
}
