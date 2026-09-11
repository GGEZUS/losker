//! ASCII-art spinner stage on the right side of the greeter.
//! The art itself is the user's choice — this module only renders frames.
//!
//! Frames file resolution (from config, see `crate::config::Logo`):
//!   - `Legacy` (default): /etc/greetd/losker-ascii.txt
//!   - `Hidden`: no art at all (caller skips the slot)
//!   - `Key(k)`: <logos_dir>/k.txt, falling back to the legacy file when
//!     missing (a mistyped key degrades to today's behavior, never errors)
//!   - `logos_dir()` honors LOSKER_LOGOS for tests/dry-runs
//! Format:
//!   - optional header `# delay: <ms>` sets the frame cadence (default 100)
//!   - one frame per block, blocks separated by a line containing only `---`
//!   - blank blocks are dropped; single block = static art (no animation)
//! Until a file exists, a dim placeholder marks the slot.
//!
//! Font sizing: CSS keeps 27px for the ~50-col arch reference; wider
//! canvases (other distros) get a per-label pango size override so the
//! widest frame still fits the column.

use crate::config::{valid_logo_key, Config, Logo, LogoRes, LogoSize};
use gtk4::glib;
use gtk4::prelude::*;
use std::cell::Cell;
use std::path::{Path, PathBuf};

pub const LEGACY_FRAMES_FILE: &str = "/etc/greetd/losker-ascii.txt";
const DEFAULT_FRAME_MS: u64 = 100;
/// arch reference canvas: ~50 cols render at 27px and fill the art column
const REFERENCE_COLS: usize = 50;
const REFERENCE_PX: i32 = 27;
const MIN_PX: i32 = 9;
/// measured (nested-niri alloc trace), not guessed: a DejaVu Sans Mono line
/// box is ~1.165 × the font size (glyph extent) and `.ascii-art` adds
/// line-height 1.15 → 19 rows @27px measured 687px. The hero class tightens
/// line-height to 1.05 → 1.165 × 1.05 ≈ HERO_LINE_F.
const HERO_LINE_F: f64 = 1.23;
/// monospace advance width in em (measured: 40 cols @27px = 651px wide)
const HERO_CHAR_W: f64 = 0.61;

/// Pure logos-dir resolution — tests never touch the process env.
pub fn resolve_logos_dir(env_val: Option<&str>) -> PathBuf {
    match env_val {
        Some(v) if !v.trim().is_empty() => PathBuf::from(v),
        _ => PathBuf::from("/etc/losker/logos"),
    }
}

pub fn logos_dir() -> PathBuf {
    resolve_logos_dir(std::env::var("LOSKER_LOGOS").ok().as_deref())
}

/// Frames-file candidates for a logo at a given tier, best first. `High` is
/// the plain capture (no suffix); lower tiers are suffixed files that fall
/// back to the plain capture when that tier was never baked. Legacy keeps
/// its greeting-dir path and takes the suffix inline. Pure — unit-testable.
pub fn frames_candidates_in(dir: &Path, logo: &Logo, res: LogoRes) -> Vec<PathBuf> {
    let base: PathBuf = match logo {
        Logo::Legacy => PathBuf::from(LEGACY_FRAMES_FILE),
        Logo::Hidden => return Vec::new(),
        Logo::Key(k) if valid_logo_key(k) => dir.join(format!("{k}.txt")),
        Logo::Key(_) => PathBuf::from(LEGACY_FRAMES_FILE),
    };
    let with_suffix = |sfx: &str| -> PathBuf {
        match base.to_string_lossy().strip_suffix(".txt") {
            Some(stem) => PathBuf::from(format!("{stem}{sfx}.txt")),
            None => base.clone(),
        }
    };
    match res {
        LogoRes::High => vec![with_suffix("")],
        LogoRes::Medium => vec![with_suffix("-med"), with_suffix("")],
        LogoRes::Low => vec![with_suffix("-low"), with_suffix("")],
    }
}

pub fn frames_candidates(logo: &Logo, res: LogoRes) -> Vec<PathBuf> {
    frames_candidates_in(&logos_dir(), logo, res)
}

/// Per-label font size so the widest frame fits the same column the 50-col
/// arch art fills at 27px. Never grows past the reference (narrow art just
/// gets more air); floors at MIN_PX so degenerate canvases stay legible.
pub fn font_px(frames: &[String]) -> i32 {
    let cols = frames
        .iter()
        .flat_map(|f| f.lines())
        .map(|l| l.chars().count()) // not len(): art may hold multi-byte chars
        .max()
        .unwrap_or(REFERENCE_COLS);
    if cols == 0 {
        return REFERENCE_PX;
    }
    let px = ((REFERENCE_COLS as f64 * REFERENCE_PX as f64) / cols as f64).floor() as i32;
    px.clamp(MIN_PX, REFERENCE_PX)
}

/// canvas (cols, rows) of the widest/tallest frame — rows drive the hero fit
fn canvas_size(frames: &[String]) -> (usize, usize) {
    let mut cols = 0usize;
    let mut rows = 0usize;
    for f in frames {
        rows = rows.max(f.lines().count());
        for l in f.lines() {
            cols = cols.max(l.chars().count()); // chars(): art may be multi-byte
        }
    }
    (cols, rows)
}

/// Largest px whose rendered box fits (max_w, max_h) — the hero fit. Both
/// budgets bind: narrow monitors shrink by width, tall ones by height.
/// Degenerate canvases keep the reference size. No upper cap by design:
/// res × size is the user's pairing, and a low-res canvas blown up large
/// is an informed choice, not a bug to silently shrink away.
pub fn font_px_fit(frames: &[String], max_w: i32, max_h: i32) -> i32 {
    let (cols, rows) = canvas_size(frames);
    if cols == 0 || rows == 0 {
        return REFERENCE_PX;
    }
    let px_w = max_w as f64 / (cols as f64 * HERO_CHAR_W);
    let px_h = max_h as f64 / (rows as f64 * HERO_LINE_F);
    (px_w.min(px_h).floor() as i32).max(MIN_PX)
}

/// On-screen fit box for the logo: the screen-safe area — status line and
/// dock tab band stay clear, since a CENTERED logo must never reach the
/// arrow's territory — scaled by the chosen size. Pure: the TUI computes
/// pixel readouts with this, without a display.
pub fn art_budget_for(screen: (i32, i32), size: LogoSize) -> (i32, i32) {
    let safe_h = (screen.1 - 16 - 20 - 42 - 44).max(240);
    let safe_w = (screen.0 - 68 - 420 - 40).max(240);
    let f = size.factor();
    (
        ((safe_w as f64 * f) as i32).max(240),
        ((safe_h as f64 * f) as i32).max(240),
    )
}

/// Expected on-screen size (px) of this canvas inside a fit box — what the
/// views will actually draw, so the TUI can show what a choice means.
pub fn rendered_px(frames: &[String], budget: (i32, i32)) -> (i32, i32) {
    let px = font_px_fit(frames, budget.0, budget.1);
    let (cols, rows) = canvas_size(frames);
    let w = (cols as f64 * HERO_CHAR_W * px as f64).round() as i32;
    let h = (rows as f64 * HERO_LINE_F * px as f64).round() as i32;
    (w, h)
}

/// The animated art label — `None` when config hides the art entirely.
///
/// The timer closure STRONGLY owns the frames and a label clone — the first
/// version kept its state in an `Rc<AsciiSpin>` held only by `build()`'s
/// locals, so a Weak in the timer died the moment building finished and the
/// art froze on frame 0 (boot-proven).
pub fn spin_label(cfg: &Config) -> Option<gtk4::Label> {
    spin_label_sized(cfg, None)
}

/// Sized variant: `fit = Some((max_w, max_h))` sizes the art to exactly
/// fill that box (`ascii-hero` class, tighter leading) instead of the flat
/// 27px column cap.
pub fn spin_label_sized(cfg: &Config, fit: Option<(i32, i32)>) -> Option<gtk4::Label> {
    let label = gtk4::Label::new(None);
    label.add_css_class("ascii-art");
    if fit.is_some() {
        label.add_css_class("ascii-hero"); // line-height 1.05 (see HERO_LINE_F)
    }
    label.set_valign(gtk4::Align::Center);
    label.set_halign(gtk4::Align::Center);

    let (frames, frame_ms) = load_frames(&cfg.logo, cfg.logo_res);

    match frames.len() {
        0 => label.set_text("[ ascii slot ]"),
        1 => {
            label.set_text(&frames[0]);
            apply_font_px(&label, sized_px(&frames, fit));
        }
        _ => {
            label.set_text(&frames[0]);
            apply_font_px(&label, sized_px(&frames, fit));
            let label = label.clone();
            let idx = Cell::new(0usize);
            glib::timeout_add_local(std::time::Duration::from_millis(frame_ms), move || {
                let i = idx.get();
                label.set_text(&frames[i % frames.len()]);
                idx.set((i + 1) % frames.len());
                glib::ControlFlow::Continue
            });
        }
    }
    Some(label)
}

/// px for this label: no fit = flat column cap; fit = fill the given box
fn sized_px(frames: &[String], fit: Option<(i32, i32)>) -> i32 {
    match fit {
        Some((w, h)) => font_px_fit(frames, w, h),
        None => font_px(frames),
    }
}

/// Frames + cadence for a logo choice at a tier: the best candidate that
/// reads wins (tier → plain capture → legacy file for keys). No error
/// surface — the slot placeholder and the trace cover diagnostics.
fn load_frames(logo: &Logo, tier: LogoRes) -> (Vec<String>, u64) {
    for path in frames_candidates(logo, tier) {
        if let Ok(frames) = std::fs::read_to_string(&path).map(|text| parse_frames(&text)) {
            return frames;
        }
    }
    if matches!(logo, Logo::Key(_)) {
        crate::auth::trace("ascii: no tier of this logo readable — legacy fallback");
        if let Ok(frames) = std::fs::read_to_string(LEGACY_FRAMES_FILE).map(|t| parse_frames(&t)) {
            return frames;
        }
    }
    (Vec::new(), DEFAULT_FRAME_MS)
}

/// Override the CSS 27px whenever the computed size differs — shrink (wide
/// canvases in the greeter column) or grow (hero fit). Absolute pango size
/// == CSS px semantics; the reference path stays pure-CSS.
fn apply_font_px(label: &gtk4::Label, px: i32) {
    if px == REFERENCE_PX {
        return;
    }
    let attrs = gtk4::pango::AttrList::new();
    let mut desc = gtk4::pango::FontDescription::new();
    desc.set_absolute_size(px as f64 * gtk4::pango::SCALE as f64);
    attrs.insert(gtk4::pango::AttrFontDesc::new(&desc));
    label.set_attributes(Some(&attrs));
}

/// Parse the frames file: optional `# delay: <ms>` header, then blocks
/// separated by `---` lines. Blank blocks are dropped. Pure — unit-testable.
pub fn parse_frames(text: &str) -> (Vec<String>, u64) {
    let mut delay_ms = DEFAULT_FRAME_MS;
    let mut frames: Vec<String> = Vec::new();
    let mut cur: Vec<&str> = Vec::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("# delay:") {
            if let Ok(d) = rest.trim().parse::<u64>() {
                // sane floor: below ~60fps the label churn outpaces repaints
                delay_ms = d.clamp(16, 1000);
            }
        } else if line.trim() == "---" {
            frames.push(cur.join("\n"));
            cur.clear();
        } else {
            cur.push(line);
        }
    }
    if !cur.is_empty() {
        frames.push(cur.join("\n"));
    }
    (
        frames.into_iter().filter(|f| !f.trim().is_empty()).collect(),
        delay_ms,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_blocks_and_reads_delay_header() {
        let text = "# delay: 50\n AAA \n---\n BBB \n";
        let (frames, delay) = parse_frames(text);
        // leading/trailing spaces are art positioning — preserved verbatim
        assert_eq!(frames, vec![" AAA ".to_string(), " BBB ".to_string()]);
        assert_eq!(delay, 50);
    }

    #[test]
    fn frames_candidates_fall_back_to_the_plain_capture() {
        let dir = Path::new("/logos");
        let key = Logo::Key("cachyos".into());
        assert_eq!(
            frames_candidates_in(dir, &key, LogoRes::High),
            vec![PathBuf::from("/logos/cachyos.txt")]
        );
        // a tier that was never baked degrades to the plain capture
        assert_eq!(
            frames_candidates_in(dir, &key, LogoRes::Medium),
            vec![
                PathBuf::from("/logos/cachyos-med.txt"),
                PathBuf::from("/logos/cachyos.txt")
            ]
        );
        assert_eq!(
            frames_candidates_in(dir, &key, LogoRes::Low),
            vec![
                PathBuf::from("/logos/cachyos-low.txt"),
                PathBuf::from("/logos/cachyos.txt")
            ]
        );
        // legacy keeps its greeting-dir path, suffix inline
        assert_eq!(
            frames_candidates_in(dir, &Logo::Legacy, LogoRes::Medium),
            vec![
                PathBuf::from("/etc/greetd/losker-ascii-med.txt"),
                PathBuf::from("/etc/greetd/losker-ascii.txt")
            ]
        );
        assert!(frames_candidates_in(dir, &Logo::Hidden, LogoRes::High).is_empty());
        // unsanitary key (impossible via the parser, but guarded) → legacy
        assert_eq!(
            frames_candidates_in(dir, &Logo::Key("../etc".into()), LogoRes::High),
            vec![PathBuf::from(LEGACY_FRAMES_FILE)]
        );
    }

    #[test]
    fn logos_dir_env_resolution() {
        assert_eq!(
            resolve_logos_dir(Some("/tmp/logos")),
            PathBuf::from("/tmp/logos")
        );
        assert_eq!(
            resolve_logos_dir(Some("")),
            PathBuf::from("/etc/losker/logos")
        );
        assert_eq!(
            resolve_logos_dir(None),
            PathBuf::from("/etc/losker/logos")
        );
    }

    #[test]
    fn font_px_scales_down_and_never_up() {
        let wide = |cols: usize| vec!["x".repeat(cols)];
        // 50-col reference → untouched 27px
        assert_eq!(font_px(&wide(50)), 27);
        // wider canvases shrink to keep the column width
        assert_eq!(font_px(&wide(60)), 22);
        assert_eq!(font_px(&wide(100)), 13);
        // degenerate canvases floor at MIN_PX, never grow past the reference
        assert_eq!(font_px(&wide(200)), 9);
        assert_eq!(font_px(&wide(30)), 27);
        assert_eq!(font_px(&[]), 27);
        // multi-byte art chars count as one column (chars(), not bytes)
        assert_eq!(font_px(&vec!["█".repeat(100)]), 13);
    }

    #[test]
    fn font_px_fit_binds_on_the_tighter_axis() {
        // one frame = the whole canvas as ONE newline-joined string
        // (parse_frames shape — canvas_size counts rows per frame)
        let canvas = |rows: usize, cols: usize| {
            vec![(0..rows)
                .map(|_| "x".repeat(cols))
                .collect::<Vec<_>>()
                .join("\n")]
        };
        // arch-shaped 40x19 in a Surface-sized box: height binds at 27
        // (636px slot / 19 rows / 1.23 line factor), width is far away
        assert_eq!(font_px_fit(&canvas(19, 40), 1232, 636), 27);
        // same canvas, huge slot: no upper cap — res × size is the user's
        // pairing, so the fit grows as far as the axes allow (width binds)
        assert_eq!(
            font_px_fit(&canvas(19, 40), 8000, 8000),
            ((8000.0 / (40.0 * HERO_CHAR_W)).floor()) as i32
        );
        // wide canvas on a narrow (portrait) monitor: width binds
        let px = font_px_fit(&canvas(24, 58), 372, 3000);
        assert_eq!(px, ((372.0 / (58.0 * HERO_CHAR_W)).floor()) as i32);
        // degenerate canvases floor at MIN_PX and fall back when empty
        assert_eq!(font_px_fit(&canvas(19, 40), 10, 10), MIN_PX);
        assert_eq!(font_px_fit(&[], 1232, 636), REFERENCE_PX);
    }

    #[test]
    fn no_header_means_default_delay() {
        let (frames, delay) = parse_frames("only\n");
        assert_eq!(frames, vec!["only".to_string()]);
        assert_eq!(delay, DEFAULT_FRAME_MS);
    }

    #[test]
    fn shading_hash_lines_are_art_not_headers() {
        // the donut ramp uses '#' mid-art; only the exact `# delay:` prefix
        // is a directive
        let (frames, _) = parse_frames("#$@!\n~~\n");
        assert_eq!(frames, vec!["#$@!\n~~".to_string()]);
    }

    #[test]
    fn blank_blocks_are_dropped() {
        let text = "A\n---\n\n \n---\nB\n";
        let (frames, _) = parse_frames(text);
        assert_eq!(frames, vec!["A".to_string(), "B".to_string()]);
    }

    #[test]
    fn delay_is_clamped_to_sane_bounds() {
        let (_, d0) = parse_frames("# delay: 1\nx\n");
        let (_, d1) = parse_frames("# delay: 9999\nx\n");
        assert_eq!((d0, d1), (16, 1000));
    }
}
