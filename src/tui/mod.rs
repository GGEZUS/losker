//! `losker --config` — the ratatui TUI helper. Owns the terminal
//! (raw mode + alternate screen), so it must run before any gtk init.
//!
//! Structure: `state::Model` is a pure keystroke→(state, Action) machine;
//! this module executes the actions (save, logo generation in a worker
//! thread) and drives the render loop. The effective config path is always
//! on screen — `sudo` strips LOSKER_CONFIG/LOSKER_LOGOS, and the
//! footer is how a "dry-run" that suddenly isn't gets noticed.

mod state;
mod view;

use crate::config::{self, Logo};
use state::{Action, Model};
use std::io::Write;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

pub fn run() -> i32 {
    use crossterm::tty::IsTty;
    if !std::io::stdout().is_tty() {
        eprintln!("losker --config needs a terminal (it takes over the screen)");
        return 1;
    }

    let cfg = config::load();
    let config_path = config::path();
    let logos_dir = crate::ui::ascii::logos_dir();
    let logos = scan_logos(&logos_dir);
    let screen = probe_screen();
    let mut model = Model::new(cfg, logos, config_path.clone(), logos_dir.clone(), screen);
    reload_preview(&mut model, 80);

    // panic safety: restore the terminal before the default hook prints
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
        prev_hook(info);
    }));

    if let Err(e) = crossterm::terminal::enable_raw_mode() {
        eprintln!("losker: cannot enable raw mode: {e}");
        return 1;
    }
    let mut stdout = std::io::stdout();
    let _ = crossterm::execute!(
        stdout,
        crossterm::terminal::EnterAlternateScreen,
        crossterm::cursor::Hide
    );

    let mut terminal = match ratatui::Terminal::new(ratatui::backend::CrosstermBackend::new(&mut stdout)) {
        Ok(t) => t,
        Err(e) => {
            restore_terminal();
            eprintln!("losker: terminal setup failed: {e}");
            return 1;
        }
    };

    // generation worker replies (house idiom: thread + mpsc, like the auth
    // outcomes in both UIs — never block the loop on the ~15 s pty capture)
    let (gen_tx, gen_rx) = mpsc::channel();
    let mut last_tick = Instant::now();
    let exit_code = loop {
        // keystrokes → state + actions
        let mut quit = false;
        if crossterm::event::poll(Duration::from_millis(80)).unwrap_or(false) {
            if let Ok(ev) = crossterm::event::read() {
                if let crossterm::event::Event::Key(key) = ev {
                    if key.kind == crossterm::event::KeyEventKind::Press {
                        for a in model.key(&key_name(key)) {
                            match a {
                                Action::Quit => quit = true,
                                Action::Save => do_save(&mut model),
                                Action::Generate { key, out, res } => {
                                    spawn_generate(&model, key, out, res, gen_tx.clone())
                                }
                                Action::RefreshLogos => {
                                    model.logos = scan_logos(&logos_dir);
                                    if let Some(i) =
                                        model.logos.iter().position(|l| l == &model.cfg.logo)
                                    {
                                        model.logo_pos = i;
                                    }
                                    model.preview_stale = true;
                                }
                            }
                        }
                        if quit {
                            break 0;
                        }
                    }
                }
            }
        }

        // finished generations surface in the status line
        while let Ok(msg) = gen_rx.try_recv() {
            match msg {
                GenMsg::Done(key, note) => {
                    model.status = format!("generated {key} — {note}");
                    model.logos = scan_logos(&logos_dir);
                    if let Some(i) = model.logos.iter().position(|l| l == &model.cfg.logo) {
                        model.logo_pos = i;
                    }
                    model.preview_stale = true;
                }
                GenMsg::Failed(key, note) => model.status = format!("generation failed for {key}: {note}"),
                GenMsg::Missing => {
                    model.status =
                        String::from("losker-genart not found — see README (install tools/gen-ascii-art.py)")
                }
            }
        }

        if model.preview_stale {
            // right pane is 56% of the terminal, minus its border
            let pane_cols = terminal
                .size()
                .map(|s| (s.width as f32 * 0.56) as usize)
                .unwrap_or(60)
                .saturating_sub(2);
            reload_preview(&mut model, pane_cols);
        }

        // preview cadence tick
        let cadence = Duration::from_millis(model.preview_ms.max(50));
        if last_tick.elapsed() >= cadence {
            model.tick();
            last_tick = Instant::now();
        }

        let _ = terminal.draw(|f| view::render(f, &model));
    };

    restore_terminal();
    exit_code
}

fn do_save(model: &mut Model) {
    match config::save_to(&model.cfg, &model.config_path) {
        Ok(()) => {
            model.dirty = false;
            model.status = format!(
                "saved {} (applies at next greeter/lock start)",
                model.config_path.display()
            );
        }
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            model.status = format!(
                "cannot write {} — try: sudo losker --config",
                model.config_path.display()
            );
        }
        Err(e) => model.status = format!("save failed: {e}"),
    }
}

fn spawn_generate(
    _model: &Model,
    key: String,
    out: PathBuf,
    res: crate::config::LogoRes,
    tx: mpsc::Sender<GenMsg>,
) {
    std::thread::spawn(move || {
        let res = std::process::Command::new("losker-genart")
            .args(["--logo", &key, "--size", res.capture_size(), "--out"])
            .arg(&out)
            .output();
        let _ = tx.send(match res {
            Ok(o) if o.status.success() => {
                let note = String::from_utf8_lossy(&o.stderr)
                    .lines()
                    .last()
                    .unwrap_or("done")
                    .to_string();
                GenMsg::Done(key, note)
            }
            Ok(o) => {
                let note = String::from_utf8_lossy(&o.stderr)
                    .lines()
                    .last()
                    .unwrap_or("unknown error")
                    .to_string();
                GenMsg::Failed(key, note)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => GenMsg::Missing,
            Err(e) => GenMsg::Failed(key, e.to_string()),
        });
    });
}

/// [Legacy, Hidden] + one Key per *.txt in the logos dir (sorted). The
/// baked tier variants (`<key>-med.txt`, `<key>-low.txt`) are not logos —
/// they attach to `<key>` via the resolution setting.
fn scan_logos(dir: &std::path::Path) -> Vec<Logo> {
    let mut keys: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.path().file_stem().map(|s| s.to_string_lossy().into_owned()))
        .filter(|k| !k.ends_with("-med") && !k.ends_with("-low"))
        .filter(|k| crate::config::valid_logo_key(k))
        .collect();
    keys.sort();
    keys.dedup();
    let mut logos = vec![Logo::Legacy, Logo::Hidden];
    logos.extend(keys.into_iter().map(Logo::Key));
    logos
}

/// Primary monitor geometry, for the pixel readouts. The TUI never draws
/// windows, so a plain gtk init (no application, no main loop) is only a
/// display query; without a display (SSH, stripped sudo env) the size row
/// degrades to percentages.
fn probe_screen() -> Option<(i32, i32)> {
    use gtk4::prelude::*;
    if !gtk4::is_initialized() && gtk4::init().is_err() {
        return None;
    }
    crate::ui::primary_monitor().map(|m| {
        let g = m.geometry();
        (g.width(), g.height())
    })
}

/// Preview loader: the pane is a fraction of the screen, so the configured
/// tier (drawn full-screen by the real views) may be far wider than it.
/// Preference: the CONFIGURED tier when it fits, else the widest tier that
/// does — and the meta line names what is up, so a fallback never reads as
/// a dead setting. Also refreshes the logo_size pixel readout, which always
/// reflects the CONFIGURED tier's canvas (what the views will draw).
fn reload_preview(model: &mut Model, max_cols: usize) {
    // size readout for the configured logo+tier+size on the probed screen
    model.size_px = crate::ui::ascii::frames_candidates(model.current_logo(), model.cfg.logo_res)
        .iter()
        .find_map(|p| std::fs::read_to_string(p).ok())
        .map(|t| {
            let (frames, _) = crate::ui::ascii::parse_frames(&t);
            let budget = crate::ui::ascii::art_budget_for(
                model.screen.unwrap_or((1920, 1080)),
                model.cfg.logo_size,
            );
            crate::ui::ascii::rendered_px(&frames, budget)
        });

    let cfg_tier = model.cfg.logo_res;
    let mut order = vec![cfg_tier];
    for t in [
        crate::config::LogoRes::High,
        crate::config::LogoRes::Medium,
        crate::config::LogoRes::Low,
    ] {
        if t != cfg_tier {
            order.push(t);
        }
    }
    let mut clipped: Option<(Vec<String>, u64, crate::config::LogoRes)> = None;
    for tier in order {
        for path in crate::ui::ascii::frames_candidates(model.current_logo(), tier) {
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let (frames, ms) = crate::ui::ascii::parse_frames(&text);
            let widest = frames
                .iter()
                .flat_map(|f| f.lines())
                .map(|l| l.chars().count())
                .max()
                .unwrap_or(0);
            if widest <= max_cols {
                model.set_preview(frames, ms);
                model.preview_tier = tier;
                model.preview_note = if tier == cfg_tier {
                    None
                } else {
                    Some(format!(
                        "pane too narrow for {} — showing {}",
                        cfg_tier.name(),
                        tier.name()
                    ))
                };
                return;
            }
            clipped = Some((frames, ms, tier));
        }
    }
    match clipped {
        Some((frames, ms, tier)) => {
            model.set_preview(frames, ms);
            model.preview_tier = tier;
            model.preview_note = Some(format!("pane too narrow — {} clipped", cfg_tier.name()));
        }
        None => {
            model.set_preview(Vec::new(), 100);
            model.preview_tier = cfg_tier;
            model.preview_note = None;
        }
    }
}

/// crossterm KeyEvent → the Model's normalized key name
fn key_name(key: crossterm::event::KeyEvent) -> String {
    use crossterm::event::KeyCode;
    match key.code {
        KeyCode::Up => "up".into(),
        KeyCode::Down => "down".into(),
        KeyCode::Left => "left".into(),
        KeyCode::Right => "right".into(),
        KeyCode::Enter => "enter".into(),
        KeyCode::Esc => "esc".into(),
        KeyCode::Backspace => "backspace".into(),
        KeyCode::Char(' ') => " ".into(),
        KeyCode::Char(c) => c.to_string(),
        _ => String::new(),
    }
}

fn restore_terminal() {
    let _ = crossterm::terminal::disable_raw_mode();
    let mut out = std::io::stdout();
    let _ = crossterm::execute!(
        out,
        crossterm::terminal::LeaveAlternateScreen,
        crossterm::cursor::Show
    );
    let _ = out.flush();
}

enum GenMsg {
    Done(String, String),
    Failed(String, String),
    Missing,
}
