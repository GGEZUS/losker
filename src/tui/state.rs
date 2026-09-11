//! Pure state machine for the `--config` TUI: no terminal, no gtk, no I/O —
//! `key()` maps keystrokes to state changes + `Action`s, and every test
//! lives here. The event loop in `super` executes the actions.

use crate::config::{valid_logo_key, Config, Crt, HexColor, Logo, LogoRes, LogoSize};
use std::path::PathBuf;

/// Accent presets cycling order. Stock-green first; deny-red deliberately
/// absent (an accent equal to ACCESS-DENIED red would blur semantics).
pub const ACCENT_PRESETS: [Option<HexColor>; 5] = [
    None,
    Some(HexColor { r: 0xff, g: 0xb0, b: 0x00 }), // amber
    Some(HexColor { r: 0x7f, g: 0xd7, b: 0xff }), // ice
    Some(HexColor { r: 0xff, g: 0x5f, b: 0xd2 }), // magenta
    Some(HexColor { r: 0x22, g: 0xd3, b: 0xee }), // cyan
];

const CRT_LEVELS: [Crt; 3] = [Crt::Strong, Crt::Subtle, Crt::Off];
const RES_LEVELS: [LogoRes; 3] = [LogoRes::High, LogoRes::Medium, LogoRes::Low];
const SIZE_LEVELS: [LogoSize; 3] = [LogoSize::Large, LogoSize::Medium, LogoSize::Small];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Osk,
    Logo,
    Res,
    Size,
    Accent,
    Crt,
}

const FOCUS_ORDER: [Focus; 6] = [
    Focus::Osk,
    Focus::Logo,
    Focus::Res,
    Focus::Size,
    Focus::Accent,
    Focus::Crt,
];

/// What the event loop must do after a keystroke.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Quit,
    Save,
    /// run the generator (losker-genart) for this logo key at this tier
    Generate {
        key: String,
        out: PathBuf,
        res: LogoRes,
    },
    /// re-scan the logos dir after a generation finished
    RefreshLogos,
}

pub struct Model {
    pub cfg: Config,
    pub focus: Focus,
    /// [Legacy, Hidden, Key(..)…] — injected, never scanned by pure code
    pub logos: Vec<Logo>,
    pub logo_pos: usize,
    pub accent_idx: usize,
    /// preview frames + cadence of the selected logo (loaded by the loop)
    pub preview: Vec<String>,
    pub preview_ms: u64,
    pub preview_stale: bool,
    /// tier actually loaded into the preview — the pane prefers the
    /// configured one but falls back when it can't fit (view.rs meta line
    /// shows this + the note, so a fallback never reads as a dead setting)
    pub preview_tier: LogoRes,
    pub preview_note: Option<String>,
    /// primary monitor geometry, probed at startup (None: no display — the
    /// size row falls back to percentages instead of pixels)
    pub screen: Option<(i32, i32)>,
    /// expected on-screen size (px) of the configured logo+tier+size — the
    /// informed-decision readout on the logo_size row
    pub size_px: Option<(i32, i32)>,
    pub frame: usize,
    /// paths the footer shows — the sudo/env trap defense
    pub config_path: PathBuf,
    pub logos_dir: PathBuf,
    pub status: String,
    /// Some(buffer) = custom-logo entry mode
    pub input: Option<String>,
    pub dirty: bool,
}

impl Model {
    pub fn new(
        cfg: Config,
        logos: Vec<Logo>,
        config_path: PathBuf,
        logos_dir: PathBuf,
        screen: Option<(i32, i32)>,
    ) -> Self {
        let logo_pos = logos.iter().position(|l| l == &cfg.logo).unwrap_or(0);
        let accent_idx = ACCENT_PRESETS
            .iter()
            .position(|a| *a == cfg.accent)
            // custom (non-preset) accent: keep it, shown as-is until cycled
            .unwrap_or(0);
        Self {
            cfg,
            focus: Focus::Osk,
            logos,
            logo_pos,
            accent_idx,
            preview: Vec::new(),
            preview_ms: 100,
            preview_stale: true,
            preview_tier: LogoRes::High,
            preview_note: None,
            screen,
            size_px: None,
            frame: 0,
            config_path,
            logos_dir,
            status: String::from("ready — edits apply at next greeter/lock start"),
            input: None,
            dirty: false,
        }
    }

    /// One keystroke → state change + actions. `k` is a normalized name:
    /// "up" "down" "left" "right" "enter" "esc" "backspace" " " or a char.
    pub fn key(&mut self, k: &str) -> Vec<Action> {
        // custom-logo entry mode eats everything first
        if self.input.is_some() {
            return self.key_input(k);
        }
        match k {
            "q" | "esc" => vec![Action::Quit],
            "up" | "k" => {
                self.focus = focus_step(self.focus, -1);
                vec![]
            }
            "down" | "j" => {
                self.focus = focus_step(self.focus, 1);
                vec![]
            }
            "left" | "h" => {
                self.value_prev();
                vec![]
            }
            "right" | "l" => {
                self.value_next();
                vec![]
            }
            " " | "enter" => self.activate(),
            "s" => vec![Action::Save],
            "g" => {
                self.input = Some(String::new());
                self.status = String::from("logo key (fastfetch --list-logos), ENTER to generate:");
                vec![]
            }
            _ => vec![],
        }
    }

    fn key_input(&mut self, k: &str) -> Vec<Action> {
        let buf = self.input.as_mut().expect("input mode");
        match k {
            "esc" => {
                self.input = None;
                self.status = String::from("input cancelled");
            }
            "backspace" => {
                buf.pop();
            }
            "enter" => {
                let key = std::mem::take(buf);
                self.input = None;
                return self.accept_logo(key);
            }
            other if other.chars().count() == 1 => {
                // only characters a logo key may contain — everything else
                // is silently dropped (validation happens again on accept)
                if let Some(c) = other
                    .chars()
                    .next()
                    .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
                {
                    if buf.len() < 32 {
                        buf.push(c);
                    }
                }
            }
            _ => {}
        }
        vec![]
    }

    /// ENTER on the logo input: select (or add + select) the key and ask the
    /// loop to generate its frames.
    fn accept_logo(&mut self, key: String) -> Vec<Action> {
        if !valid_logo_key(&key) {
            self.status = format!("invalid logo key: \"{key}\"");
            return vec![];
        }
        if let Some(i) = self.logos.iter().position(|l| matches!(l, Logo::Key(k) if *k == key)) {
            self.logo_pos = i;
        } else {
            self.logos.push(Logo::Key(key.clone()));
            self.logo_pos = self.logos.len() - 1;
        }
        self.cfg.logo = Logo::Key(key.clone());
        self.dirty = true;
        self.preview_stale = true;
        self.status = format!("generating {key} at {} res (~15 s)…", self.cfg.logo_res.name());
        vec![Action::Generate {
            out: self
                .logos_dir
                .join(format!("{key}{}.txt", self.cfg.logo_res.suffix())),
            key,
            res: self.cfg.logo_res,
        }]
    }

    fn value_prev(&mut self) {
        match self.focus {
            Focus::Osk => self.set_osk(true),
            Focus::Logo => {
                self.logo_step(-1);
            }
            Focus::Res => {
                self.res_step(-1);
            }
            Focus::Size => {
                self.size_step(-1);
            }
            Focus::Accent => {
                self.accent_step(-1);
            }
            Focus::Crt => {
                self.crt_step(-1);
            }
        }
    }

    fn value_next(&mut self) {
        match self.focus {
            Focus::Osk => self.set_osk(false),
            Focus::Logo => {
                self.logo_step(1);
            }
            Focus::Res => {
                self.res_step(1);
            }
            Focus::Size => {
                self.size_step(1);
            }
            Focus::Accent => {
                self.accent_step(1);
            }
            Focus::Crt => {
                self.crt_step(1);
            }
        }
    }

    fn activate(&mut self) -> Vec<Action> {
        match self.focus {
            Focus::Osk => {
                self.set_osk(!self.cfg.osk);
                vec![]
            }
            Focus::Logo => self.logo_step(1),
            Focus::Res => self.res_step(1),
            Focus::Size => self.size_step(1),
            Focus::Accent => self.accent_step(1),
            Focus::Crt => self.crt_step(1),
        }
    }

    fn set_osk(&mut self, on: bool) {
        if self.cfg.osk != on {
            self.cfg.osk = on;
            self.dirty = true;
            if on {
                self.status = String::from("osk enabled");
            } else {
                self.status = String::from(
                    "osk disabled — needs a physical keyboard at the greeter AND lock",
                );
            }
        }
    }

    fn logo_step(&mut self, dir: i32) -> Vec<Action> {
        if self.logos.is_empty() {
            return vec![];
        }
        let n = self.logos.len();
        self.logo_pos = (self.logo_pos as i32 + dir).rem_euclid(n as i32) as usize;
        self.cfg.logo = self.logos[self.logo_pos].clone();
        self.dirty = true;
        self.preview_stale = true;
        vec![]
    }

    fn res_step(&mut self, dir: i32) -> Vec<Action> {
        let cur = RES_LEVELS.iter().position(|r| *r == self.cfg.logo_res).unwrap_or(0);
        let n = RES_LEVELS.len() as i32;
        self.cfg.logo_res = RES_LEVELS[(cur as i32 + dir).rem_euclid(n) as usize];
        self.dirty = true;
        // the pane is narrow, so the preview picks the widest tier that fits —
        // a tier change there is only visible when the pane allows it
        self.preview_stale = true;
        vec![]
    }

    fn size_step(&mut self, dir: i32) -> Vec<Action> {
        let cur = SIZE_LEVELS
            .iter()
            .position(|s| *s == self.cfg.logo_size)
            .unwrap_or(0);
        let n = SIZE_LEVELS.len() as i32;
        self.cfg.logo_size = SIZE_LEVELS[(cur as i32 + dir).rem_euclid(n) as usize];
        self.dirty = true;
        self.preview_stale = true; // refreshes the pixel readout too
        vec![]
    }

    fn accent_step(&mut self, dir: i32) -> Vec<Action> {
        let n = ACCENT_PRESETS.len() as i32;
        self.accent_idx = (self.accent_idx as i32 + dir).rem_euclid(n) as usize;
        self.cfg.accent = ACCENT_PRESETS[self.accent_idx];
        self.dirty = true;
        vec![]
    }

    fn crt_step(&mut self, dir: i32) -> Vec<Action> {
        let cur = CRT_LEVELS.iter().position(|l| *l == self.cfg.crt).unwrap_or(0);
        let n = CRT_LEVELS.len() as i32;
        self.cfg.crt = CRT_LEVELS[(cur as i32 + dir).rem_euclid(n) as usize];
        self.dirty = true;
        vec![]
    }

    /// The event loop reloads the preview when the logo choice changed.
    pub fn set_preview(&mut self, frames: Vec<String>, ms: u64) {
        self.preview = frames;
        self.preview_ms = ms;
        self.preview_stale = false;
        self.frame = 0;
    }

    /// advance the preview by one frame (called at the cadence tick)
    pub fn tick(&mut self) {
        if !self.preview.is_empty() {
            self.frame = (self.frame + 1) % self.preview.len();
        }
    }

    pub fn current_logo(&self) -> &Logo {
        self.logos.get(self.logo_pos).unwrap_or(&Logo::Legacy)
    }
}

fn focus_step(cur: Focus, dir: i32) -> Focus {
    let i = FOCUS_ORDER.iter().position(|f| *f == cur).unwrap_or(0);
    let n = FOCUS_ORDER.len() as i32;
    FOCUS_ORDER[(i as i32 + dir).rem_euclid(n) as usize]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model() -> Model {
        Model::new(
            Config::default(),
            vec![
                Logo::Legacy,
                Logo::Hidden,
                Logo::Key("arch".into()),
                Logo::Key("cachyos".into()),
            ],
            PathBuf::from("/tmp/cfg.toml"),
            PathBuf::from("/tmp/logos"),
            Some((1920, 1080)),
        )
    }

    fn logo_key(m: &Model) -> String {
        match m.current_logo() {
            Logo::Key(k) => k.clone(),
            Logo::Legacy => "legacy".into(),
            Logo::Hidden => "none".into(),
        }
    }

    #[test]
    fn focus_wraps_both_directions() {
        let mut m = model();
        assert_eq!(m.focus, Focus::Osk);
        for expected in [
            Focus::Logo,
            Focus::Res,
            Focus::Size,
            Focus::Accent,
            Focus::Crt,
            Focus::Osk, // wrapped forwards
        ] {
            m.key("down");
            assert_eq!(m.focus, expected);
        }
        m.key("up");
        assert_eq!(m.focus, Focus::Crt); // wrapped backwards
    }

    #[test]
    fn logo_res_cycles_and_marks_preview_stale() {
        let mut m = model();
        m.key("down");
        m.key("down"); // focus Res
        assert_eq!(m.cfg.logo_res, LogoRes::High);
        m.key("l");
        assert_eq!(m.cfg.logo_res, LogoRes::Medium);
        m.key("l");
        assert_eq!(m.cfg.logo_res, LogoRes::Low);
        m.key("l");
        assert_eq!(m.cfg.logo_res, LogoRes::High); // wrapped
        m.key("h");
        assert_eq!(m.cfg.logo_res, LogoRes::Low);
        assert!(m.dirty);
        assert!(m.preview_stale);
    }

    #[test]
    fn logo_size_cycles() {
        let mut m = model();
        m.key("down");
        m.key("down");
        m.key("down"); // focus Size
        assert_eq!(m.cfg.logo_size, LogoSize::Large);
        m.key("l");
        assert_eq!(m.cfg.logo_size, LogoSize::Medium);
        m.key("l");
        assert_eq!(m.cfg.logo_size, LogoSize::Small);
        m.key("l");
        assert_eq!(m.cfg.logo_size, LogoSize::Large); // wrapped
        assert!(m.dirty);
        assert!(m.preview_stale);
    }

    #[test]
    fn osk_toggles_and_sets_dirty() {
        let mut m = model();
        assert!(m.cfg.osk);
        m.key("enter");
        assert!(!m.cfg.osk);
        assert!(m.dirty);
        m.key("left");
        assert!(m.cfg.osk);
    }

    #[test]
    fn logo_cycles_through_all_variants() {
        let mut m = model();
        m.key("down"); // focus Logo
        let order = ["legacy", "none", "arch", "cachyos", "legacy"];
        for expected in order {
            assert_eq!(logo_key(&m), expected);
            assert!(m.key("l").is_empty());
        }
        m.key("h"); // pos 1 → 0
        m.key("h"); // pos 0 → 3 (wrapped backwards)
        assert_eq!(logo_key(&m), "cachyos");
        assert!(m.dirty);
        assert!(m.preview_stale);
    }

    #[test]
    fn crt_and_accent_cycle() {
        let mut m = model();
        for _ in 0..5 {
            m.key("down"); // focus Crt
        }
        assert_eq!(m.cfg.crt, Crt::Strong);
        m.key(" ");
        assert_eq!(m.cfg.crt, Crt::Subtle);
        m.key(" ");
        assert_eq!(m.cfg.crt, Crt::Off);
        m.key(" ");
        assert_eq!(m.cfg.crt, Crt::Strong);

        m.key("up"); // focus Accent
        assert_eq!(m.cfg.accent, None);
        m.key("right");
        assert_eq!(m.cfg.accent, ACCENT_PRESETS[1]);
        m.key("left");
        m.key("left");
        assert_eq!(m.cfg.accent, ACCENT_PRESETS[4]); // wrapped
    }

    #[test]
    fn save_and_quit_actions() {
        let mut m = model();
        assert_eq!(m.key("s"), vec![Action::Save]);
        assert_eq!(m.key("q"), vec![Action::Quit]);
        assert_eq!(m.key("esc"), vec![Action::Quit]);
    }

    #[test]
    fn custom_logo_input_flow() {
        let mut m = model();
        m.key("g");
        assert!(m.input.is_some());
        for k in ["v", "o", "i", "d"] {
            m.key(k);
        }
        assert_eq!(m.input.as_deref(), Some("void"));
        m.key("backspace");
        assert_eq!(m.input.as_deref(), Some("voi"));
        m.key("d");
        let actions = m.key("enter");
        assert_eq!(
            actions,
            vec![Action::Generate {
                key: "void".into(),
                out: PathBuf::from("/tmp/logos/void.txt"),
                res: LogoRes::High,
            }]
        );
        assert_eq!(logo_key(&m), "void");
        assert_eq!(m.cfg.logo, Logo::Key("void".into()));
        assert!(m.dirty);
    }

    #[test]
    fn generate_writes_the_selected_tier_and_passes_its_size() {
        let mut m = model();
        m.key("down");
        m.key("down"); // focus Res
        m.key("l"); // medium
        m.key("g");
        for k in ["v", "o", "i", "d"] {
            m.key(k);
        }
        let actions = m.key("enter");
        assert_eq!(
            actions,
            vec![Action::Generate {
                key: "void".into(),
                out: PathBuf::from("/tmp/logos/void-med.txt"),
                res: LogoRes::Medium,
            }]
        );
    }

    #[test]
    fn input_mode_rejects_bad_keys_and_cancels() {
        let mut m = model();
        m.key("g");
        m.key("/");
        m.key("x");
        m.key("1");
        assert_eq!(m.input.as_deref(), Some("x1")); // '/' dropped silently
        let actions = m.key("enter");
        assert_eq!(
            actions,
            vec![Action::Generate {
                key: "x1".into(),
                out: PathBuf::from("/tmp/logos/x1.txt"),
                res: LogoRes::High,
            }]
        );
        // cancel path leaves state untouched
        m.key("g");
        m.key("a");
        assert_eq!(m.key("esc"), vec![]); // cancels input, does NOT quit
        assert!(m.input.is_none());
        assert_eq!(logo_key(&m), "x1"); // selection from before the cancel
    }

    #[test]
    fn tick_wraps_preview_frames() {
        let mut m = model();
        m.set_preview(vec!["a".into(), "b".into()], 50);
        assert_eq!(m.frame, 0);
        m.tick();
        assert_eq!(m.frame, 1);
        m.tick();
        assert_eq!(m.frame, 0);
    }
}
