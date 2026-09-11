//! Runtime config for the greeter/locker look: `/etc/losker/config.toml`.
//!
//! Same spirit as `state.rs`: flat `key = value` lines, hand-rolled parsing
//! (deliberately no TOML dep), missing file → defaults, no error surfaced.
//! Every default equals today's behavior, so a machine without a config file
//! renders exactly as before.
//!
//! Inline comments must start with ` #` (space-hash) — an accent value itself
//! begins with `#`, so a bare `#` cannot be a comment marker here.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Where the spinning-logo frames come from. Default = Legacy (today's path).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Logo {
    /// key absent or "legacy" → /etc/greetd/losker-ascii.txt
    Legacy,
    /// "none" → no art at all
    Hidden,
    /// "<key>" → <logos_dir>/<key>.txt
    Key(String),
}

/// CRT overlay intensity. `Strong` is today's exact look.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Crt {
    Strong,
    Subtle,
    Off,
}

/// Logo canvas resolution — which pre-baked capture of a logo to draw.
/// The tiers are baked by `losker-genart --size` (high = 2x capture, the
/// finest characters; medium = the original capture; low = half of it).
/// Default High: no config draws exactly today's art.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogoRes {
    High,
    Medium,
    Low,
}

impl LogoRes {
    /// value written to config.toml
    pub fn name(self) -> &'static str {
        match self {
            LogoRes::High => "high",
            LogoRes::Medium => "medium",
            LogoRes::Low => "low",
        }
    }

    /// fetch `--size` that bakes this tier
    pub fn capture_size(self) -> &'static str {
        match self {
            LogoRes::High => "2",
            LogoRes::Medium => "1",
            LogoRes::Low => "0.5",
        }
    }

    /// file-name suffix of this tier's frames file ("", "-med", "-low")
    pub fn suffix(self) -> &'static str {
        match self {
            LogoRes::High => "",
            LogoRes::Medium => "-med",
            LogoRes::Low => "-low",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "high" => Some(LogoRes::High),
            "medium" => Some(LogoRes::Medium),
            "low" => Some(LogoRes::Low),
            _ => None,
        }
    }
}

/// How much of the screen the logo is drawn to fill — independent of the
/// capture tier (a low-res logo CAN be drawn huge; that combination is the
/// user's informed call). Large is today's exact look. The TUI shows the
/// resulting pixel size per tier so the pairing is an informed decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogoSize {
    Large,
    Medium,
    Small,
}

impl LogoSize {
    /// value written to config.toml
    pub fn name(self) -> &'static str {
        match self {
            LogoSize::Large => "large",
            LogoSize::Medium => "medium",
            LogoSize::Small => "small",
        }
    }

    /// fraction of the full (screen-safe) box this size fills
    pub fn factor(self) -> f64 {
        match self {
            LogoSize::Large => 1.0,
            LogoSize::Medium => 0.65,
            LogoSize::Small => 0.4,
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "large" => Some(LogoSize::Large),
            "medium" => Some(LogoSize::Medium),
            "small" => Some(LogoSize::Small),
            _ => None,
        }
    }
}

/// One parsed accent color. Stored parsed so bad hex never survives loading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HexColor {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl HexColor {
    /// `#rrggbb`, case-insensitive, exactly 6 hex digits.
    pub fn parse(s: &str) -> Option<Self> {
        let hex = s.strip_prefix('#')?;
        if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let n = u32::from_str_radix(hex, 16).ok()?;
        Some(Self {
            r: (n >> 16) as u8,
            g: (n >> 8) as u8,
            b: n as u8,
        })
    }

    pub fn to_hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }

    /// (r, g, b) as 0.0..=1.0 floats, for cairo.
    pub fn rgb(self) -> (f64, f64, f64) {
        (
            self.r as f64 / 255.0,
            self.g as f64 / 255.0,
            self.b as f64 / 255.0,
        )
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    /// show the touch OSK dock — the deck itself starts STOWED; the ▲ tab at
    /// the bottom center of the view raises it (disable on machines that
    /// always have a keyboard)
    pub osk: bool,
    pub logo: Logo,
    /// which baked capture tier to draw (see [`LogoRes`])
    pub logo_res: LogoRes,
    /// how much of the screen the logo fills (see [`LogoSize`])
    pub logo_size: LogoSize,
    /// phosphor accent; None = stock green
    pub accent: Option<HexColor>,
    pub crt: Crt,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            osk: true,
            logo: Logo::Legacy,
            logo_res: LogoRes::High,
            logo_size: LogoSize::Large,
            accent: None,
            crt: Crt::Strong,
        }
    }
}

/// Pure path resolution — tests never touch the process env.
pub fn resolve_path(env_val: Option<&str>) -> PathBuf {
    match env_val {
        Some(v) if !v.trim().is_empty() => PathBuf::from(v),
        _ => PathBuf::from("/etc/losker/config.toml"),
    }
}

pub fn path() -> PathBuf {
    resolve_path(std::env::var("LOSKER_CONFIG").ok().as_deref())
}

pub fn load() -> Config {
    load_from(&path())
}

pub fn load_from(path: &Path) -> Config {
    let mut cfg = Config::default();
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        // missing/unreadable file: all defaults, silent — same as state.rs
        Err(_) => return cfg,
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, val)) = line.split_once('=') else {
            continue;
        };
        // inline comments start at " #" — trim FIRST so an accent value
        // ("accent = #49ff88" → " #49ff88" before trimming) keeps its '#'
        let val = val.trim();
        let val = match val.find(" #") {
            Some(i) => val[..i].trim(),
            None => val,
        };
        let key = key.trim();
        match key {
            "osk" => match val {
                "enabled" => cfg.osk = true,
                "disabled" => cfg.osk = false,
                other => bad(key, other, "enabled | disabled"),
            },
            "logo" => match val {
                "legacy" => cfg.logo = Logo::Legacy,
                "none" => cfg.logo = Logo::Hidden,
                other if valid_logo_key(other) => cfg.logo = Logo::Key(other.to_string()),
                other => bad(key, other, "<key> | none | legacy"),
            },
            "logo_res" => match LogoRes::parse(val) {
                Some(r) => cfg.logo_res = r,
                None => bad(key, val, "high | medium | low"),
            },
            "logo_size" => match LogoSize::parse(val) {
                Some(s) => cfg.logo_size = s,
                None => bad(key, val, "large | medium | small"),
            },
            "accent" => match HexColor::parse(val) {
                Some(c) => cfg.accent = Some(c),
                None => bad(key, val, "#rrggbb"),
            },
            "crt" => match val {
                "strong" => cfg.crt = Crt::Strong,
                "subtle" => cfg.crt = Crt::Subtle,
                "off" => cfg.crt = Crt::Off,
                other => bad(key, other, "strong | subtle | off"),
            },
            _ => {} // unknown keys ignored
        }
    }
    cfg
}

fn bad(key: &str, val: &str, expected: &str) {
    crate::auth::trace(&format!(
        "config: bad {key} = \"{val}\" (want {expected}) — using the default"
    ));
}

/// Logo keys become file names under the logos dir — keep them boring.
/// Kills `..`/path traversal before a key is ever joined onto a directory.
pub fn valid_logo_key(k: &str) -> bool {
    !k.is_empty()
        && k.len() <= 32
        && !k.starts_with('.')
        && k.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// Write only non-default lines — a default config saves as an empty file,
/// and `load_from(save_to(cfg)) == cfg` always roundtrips.
pub fn save_to(cfg: &Config, path: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let mut out = String::new();
    if !cfg.osk {
        out.push_str("osk = disabled\n");
    }
    match &cfg.logo {
        Logo::Legacy => {}
        Logo::Hidden => out.push_str("logo = none\n"),
        Logo::Key(k) => out.push_str(&format!("logo = {k}\n")),
    }
    if cfg.logo_res != LogoRes::High {
        out.push_str(&format!("logo_res = {}\n", cfg.logo_res.name()));
    }
    if cfg.logo_size != LogoSize::Large {
        out.push_str(&format!("logo_size = {}\n", cfg.logo_size.name()));
    }
    if let Some(c) = cfg.accent {
        out.push_str(&format!("accent = {}\n", c.to_hex()));
    }
    match cfg.crt {
        Crt::Strong => {}
        Crt::Subtle => out.push_str("crt = subtle\n"),
        Crt::Off => out.push_str("crt = off\n"),
    }
    fs::write(path, out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("osk-cfg-{}-{name}", std::process::id()))
    }

    #[test]
    fn missing_file_is_all_defaults() {
        assert_eq!(load_from(&tmp("missing-does-not-exist")), Config::default());
    }

    #[test]
    fn parses_all_keys() {
        let p = tmp("all");
        fs::write(
            &p,
            "# comment line\nosk = disabled\nlogo = cachyos\nlogo_res = low\naccent = #FFB000\ncrt = off\n",
        )
        .unwrap();
        let cfg = load_from(&p);
        assert!(!cfg.osk);
        assert_eq!(cfg.logo, Logo::Key("cachyos".into()));
        assert_eq!(cfg.logo_res, LogoRes::Low);
        assert_eq!(cfg.accent, Some(HexColor { r: 0xff, g: 0xb0, b: 0x00 }));
        assert_eq!(cfg.crt, Crt::Off);
        fs::remove_file(&p).ok();
    }

    #[test]
    fn logo_res_tiers_parse_and_name_roundtrip() {
        for r in [LogoRes::High, LogoRes::Medium, LogoRes::Low] {
            assert_eq!(LogoRes::parse(r.name()), Some(r));
        }
        assert_eq!(LogoRes::parse("ultra"), None);
        assert_eq!(LogoRes::parse("HIGH"), None); // lowercase keys, like the rest
    }

    #[test]
    fn logo_sizes_parse_factor_and_roundtrip() {
        for s in [LogoSize::Large, LogoSize::Medium, LogoSize::Small] {
            assert_eq!(LogoSize::parse(s.name()), Some(s));
        }
        assert_eq!(LogoSize::parse("huge"), None);
        // ordered: large is the biggest share of the box
        assert!(LogoSize::Large.factor() > LogoSize::Medium.factor());
        assert!(LogoSize::Medium.factor() > LogoSize::Small.factor());
    }

    #[test]
    fn keywords_none_and_legacy_map_to_logo_variants() {
        let p = tmp("kw");
        fs::write(&p, "logo = none\n").unwrap();
        assert_eq!(load_from(&p).logo, Logo::Hidden);
        fs::write(&p, "logo = legacy\n").unwrap();
        assert_eq!(load_from(&p).logo, Logo::Legacy);
        fs::remove_file(&p).ok();
    }

    #[test]
    fn bad_values_fall_back_to_defaults() {
        let p = tmp("bad");
        fs::write(
            &p,
            "osk = maybe\naccent = ff0000\ncrt = loud\nlogo = ../etc/passwd\nlogo_res = ultra\n",
        )
        .unwrap();
        assert_eq!(load_from(&p), Config::default());
        fs::remove_file(&p).ok();
    }

    #[test]
    fn unknown_keys_and_inline_comments_are_ignored() {
        let p = tmp("unknown");
        fs::write(&p, "future = thing\nosk = disabled # trust me\n").unwrap();
        let cfg = load_from(&p);
        assert!(!cfg.osk);
        assert_eq!(cfg.logo, Logo::Legacy);
        fs::remove_file(&p).ok();
    }

    #[test]
    fn accent_survives_its_hash_prefix() {
        let p = tmp("accent");
        fs::write(&p, "accent = #49ff88\n").unwrap();
        assert_eq!(
            load_from(&p).accent,
            Some(HexColor { r: 0x49, g: 0xff, b: 0x88 })
        );
        fs::remove_file(&p).ok();
    }

    #[test]
    fn roundtrip_all_non_default() {
        let cfg = Config {
            osk: false,
            logo: Logo::Key("nixos".into()),
            logo_res: LogoRes::Low,
            logo_size: LogoSize::Small,
            accent: Some(HexColor { r: 0x7f, g: 0xd7, b: 0xff }),
            crt: Crt::Subtle,
        };
        let p = tmp("roundtrip");
        save_to(&cfg, &p).unwrap();
        assert_eq!(load_from(&p), cfg);
        let saved = fs::read_to_string(&p).unwrap();
        assert!(saved.contains("logo_res = low"), "saved: {saved}");
        assert!(saved.contains("logo_size = small"), "saved: {saved}");
        fs::remove_file(&p).ok();
    }

    #[test]
    fn default_config_saves_as_empty_file() {
        let p = tmp("empty");
        save_to(&Config::default(), &p).unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), "");
        fs::remove_file(&p).ok();
    }

    #[test]
    fn resolve_path_prefers_nonempty_env() {
        assert_eq!(resolve_path(Some("/tmp/x.toml")), PathBuf::from("/tmp/x.toml"));
        assert_eq!(
            resolve_path(Some("   ")),
            PathBuf::from("/etc/losker/config.toml")
        );
        assert_eq!(
            resolve_path(None),
            PathBuf::from("/etc/losker/config.toml")
        );
    }

    #[test]
    fn logo_keys_are_boring_file_names() {
        assert!(valid_logo_key("arch"));
        assert!(valid_logo_key("CachyOS"));
        assert!(valid_logo_key("manjaro-old"));
        assert!(!valid_logo_key(""));
        assert!(!valid_logo_key("../etc"));
        assert!(!valid_logo_key(".hidden"));
        assert!(!valid_logo_key("a/b"));
        assert!(!valid_logo_key(&"x".repeat(33)));
    }
}
