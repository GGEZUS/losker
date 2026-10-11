//! Built-in on-screen keyboard — US QWERTY, shift + symbols layers.
//! Custom-drawn: keycaps are cairo on a DrawingArea, hit-tested by touch.
//! The look must NOT depend on GTK button CSS, which proved unreliable in
//! the greeter context (buttons rendered with the fallback light theme
//! regardless of the provider). Emits events to the app only — no
//! virtual-keyboard protocol, no wvkbd.
//!
//! Per-tap cost: the unpressed deck is cached as an image (rebuilt only on
//! layer/shift/scale changes); a tap blits it and paints the one pressed
//! cap live. Slow CPUs (the touch tablet) must not re-render 44 keycaps to
//! acknowledge a keystroke.
//!
//! Layout is the typewriter staircase, not a centered grid: the letter
//! rows step right by a quarter key so A falls between Q and W and Z
//! between A and S. SHIFT and the layer key below it form a flush-left
//! rail; the action keys anchor the right edge — BKSP tops the number
//! row, ↵ ends the home row, and the space bar runs to the deck's edge.
//!
//! The deck wears the app's phosphor look: caps are near-black dyed with
//! the accent, legends glow in it, a press inverts the cap to full
//! accent, and SHIFT carries a hardware-style LED that lights dim on a
//! one-shot shift and bright (plus a glow) when the caps latch is stuck.

use super::theme;
use crate::config::HexColor;
use gtk4::{cairo, prelude::*};
use std::cell::RefCell;
use std::rc::Rc;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OskEvent {
    Char(char),
    Backspace,
    Enter,
}

const LETTERS: [&[&str]; 3] = [
    &["q", "w", "e", "r", "t", "y", "u", "i", "o", "p"],
    &["a", "s", "d", "f", "g", "h", "j", "k", "l", ";"],
    &["z", "x", "c", "v", "b", "n", "m", ",", ".", "/"],
];
const NUMBERS: &[&str] = &["1", "2", "3", "4", "5", "6", "7", "8", "9", "0"];
/// Symbols layer: together with the shifted number row (!@#$%^&*()) it
/// covers every remaining ASCII printable, grouped by kind — brackets and
/// structural, then punctuation, then the high-frequency password symbols
/// in thumb reach. No typographic extras: a lock screen types passwords,
/// not prose (the old € § ¶ † © ° … — row is gone, and so is the `\`
/// that used to appear twice).
const SYM_LETTERS: [&[&str]; 3] = [
    &["{", "}", "[", "]", "(", ")", "<", ">", "/", "\\"],
    &["-", "_", "+", "=", ":", ";", "\"", "'", "?", "!"],
    &["@", "#", "$", "%", "^", "&", "*", "`", "~", "|"],
];
const SYM_NUMBERS: &[&str] = &["!", "@", "#", "$", "%", "^", "&", "*", "(", ")"];

// metrics (logical px)
const KEY_W: f64 = 66.0;
const KEY_H: f64 = 62.0;
const GAP: f64 = 6.0;
const MOD_W: f64 = 96.0; // BKSP / layer key
/// SHIFT is narrower than the other modifiers: Z's position is exactly
/// SHIFT + gap, and every px here pushes Z further under S. 78 keeps the
/// cap clearly wider than a letter key while landing Z in the A–S gap.
const SHIFT_W: f64 = 78.0;
const ENTER_W: f64 = 140.0;
/// classic typewriter proportion (~6.7u), centered in the span right of
/// the layer key rather than stretched to the deck edge
const SPACE_W: f64 = 480.0;
const PAD: f64 = 10.0; // chassis padding around the key field
const CHASSIS_R: f64 = 8.0;
// the typewriter cascade: each LETTER row starts a quarter key further
// right (SHIFT and the layer key hold the flush-left rail instead)
const STAGGER_Q: f64 = 18.0;
const STAGGER_A: f64 = 36.0;
const DEPTH: f64 = 5.0;
const RADIUS: f64 = 3.0;
/// slack for a drifting fingertip: a release off every cap but still within
/// this margin of the PRESSED cap fires it. Touch taps wander a few px
/// between down and up — without the margin a drift into a gap or just off
/// the deck edge silently swallows the key (reads as "not registered" on a
/// touchscreen). A release that lands on a DIFFERENT cap still cancels:
/// that is a deliberate slide-off, not drift.
const RELEASE_SLOP: f64 = 12.0;

/// Deck colors, derived from the configured accent so the keyboard reads
/// as part of the same terminal as everything else (None = stock green,
/// the literal ramp — same shape as `theme::token_block`).
#[derive(Clone, Copy, Debug)]
pub struct DeckPalette {
    chassis_bg: (f64, f64, f64),
    chassis_edge: (f64, f64, f64),
    cap_face: (f64, f64, f64),
    cap_side: (f64, f64, f64),
    cap_edge: (f64, f64, f64),
    mod_face: (f64, f64, f64),
    mod_side: (f64, f64, f64),
    legend: (f64, f64, f64),
    legend_dim: (f64, f64, f64),
    pressed_face: (f64, f64, f64),
    pressed_text: (f64, f64, f64),
    latch_face: (f64, f64, f64),
    latch_legend: (f64, f64, f64),
    led_off: (f64, f64, f64),
    led_once: (f64, f64, f64),
    led_on: (f64, f64, f64),
}

impl DeckPalette {
    pub fn new(accent: Option<HexColor>) -> Self {
        match accent {
            None => {
                let phos = (0.169, 0.851, 0.420); // #2bd96b
                let dim = (0.102, 0.561, 0.298); // #1a8f4c
                let hi = (0.286, 1.000, 0.533); // #49ff88
                let faint = (0.059, 0.302, 0.165); // #0f4d2a
                let black = (0.020, 0.039, 0.027); // #050a07
                DeckPalette {
                    chassis_bg: black,
                    chassis_edge: faint,
                    cap_face: (0.031, 0.075, 0.047),
                    cap_side: (0.016, 0.063, 0.031),
                    cap_edge: faint,
                    mod_face: (0.024, 0.059, 0.039),
                    mod_side: (0.012, 0.047, 0.024),
                    legend: phos,
                    legend_dim: dim,
                    pressed_face: phos,
                    pressed_text: (0.020, 0.075, 0.043),
                    latch_face: (0.051, 0.141, 0.086),
                    latch_legend: hi,
                    led_off: faint,
                    led_once: dim,
                    led_on: hi,
                }
            }
            Some(c) => DeckPalette {
                chassis_bg: theme::scale(c, 0.025),
                chassis_edge: theme::scale(c, 0.30),
                cap_face: theme::scale(c, 0.075),
                cap_side: theme::scale(c, 0.038),
                cap_edge: theme::scale(c, 0.30),
                mod_face: theme::scale(c, 0.055),
                mod_side: theme::scale(c, 0.028),
                legend: c.rgb(),
                legend_dim: theme::scale(c, 0.65),
                pressed_face: c.rgb(),
                pressed_text: theme::scale(c, 0.08),
                latch_face: theme::scale(c, 0.16),
                latch_legend: theme::mix(c, 0.30),
                led_off: theme::scale(c, 0.30),
                led_once: theme::scale(c, 0.65),
                led_on: theme::mix(c, 0.30),
            },
        }
    }
}

/// SHIFT's LED: the stuck (caps) state burns bright, a one-shot shift
/// glows dim, idle is a dark socket.
#[derive(Clone, Copy, Debug, PartialEq)]
enum LedState {
    Off,
    Once,
    Latched,
}

fn shift_led(shifted: bool, latched: bool) -> LedState {
    if latched {
        LedState::Latched
    } else if shifted {
        LedState::Once
    } else {
        LedState::Off
    }
}

#[derive(Clone, Copy, PartialEq)]
enum KeyKind {
    Normal,
    Modifier,
    Space,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum KeyAction {
    Char(char),
    Backspace,
    Enter,
    ToggleShift,
    ToggleLayer,
}

struct KeyDef {
    label: String,
    x: f64,
    y: f64,
    w: f64,
    kind: KeyKind,
    action: KeyAction,
}

struct DeckState {
    shifted: bool,
    /// caps latch: shift pressed twice stays engaged until pressed again
    shift_latched: bool,
    sym_layer: bool,
    pressed: Option<(usize, usize)>,
    layout: Vec<Vec<KeyDef>>,
    total: (f64, f64),
    pal: DeckPalette,
    on_event: Box<dyn Fn(OskEvent)>,
}

/// The unpressed deck pre-rendered into an image, so a tap costs a blit
/// plus one live keycap instead of re-rendering all ~44 caps inside a frame
/// (same trick as the CRT layer's scanline cache). Single entry: the look
/// cycles through a handful of states, and a state change pays one render —
/// exactly what a full redraw cost per tap before. The pressed inversion is
/// NEVER baked in; it is drawn live on top of the blit.
struct DeckCache {
    /// (sym_layer, shifted, shift_latched, scale factor) — everything that
    /// changes the unpressed deck's look
    key: (bool, bool, bool, i32),
    surf: cairo::ImageSurface,
}

pub struct Osk {
    pub container: gtk4::Box,
    area: gtk4::DrawingArea,
    state: Rc<RefCell<DeckState>>,
}

impl Osk {
    pub fn new(accent: Option<HexColor>, on_event: impl Fn(OskEvent) + 'static) -> Self {
        let container = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        container.add_css_class("osk");

        let area = gtk4::DrawingArea::new();
        area.set_halign(gtk4::Align::Center);
        area.set_valign(gtk4::Align::End);

        let state = Rc::new(RefCell::new(DeckState {
            shifted: false,
            shift_latched: false,
            sym_layer: false,
            pressed: None,
            layout: Vec::new(),
            total: (0.0, 0.0),
            pal: DeckPalette::new(accent),
            on_event: Box::new(on_event),
        }));

        // the deck image cache lives BESIDE DeckState, not inside it: the
        // draw func holds a Ref borrow of the state while (re)building the
        // cache, and borrow_mut-ing the same cell under it would panic
        // (same class of trap as the pressed-handler's boot-proven one)
        let cache: Rc<RefCell<Option<DeckCache>>> = Rc::new(RefCell::new(None));

        relayout(&state, &area, &cache);

        // warm the cache before the first frame: the deck's first raise
        // then blits instead of rendering 44 caps inside a frame. Scale is
        // a guess here (the window is not realized yet) — a wrong guess
        // rebuilds once, the moment the real scale is known.
        {
            let st = state.borrow();
            if let Some(surf) = render_deck_unpressed(&st, 1) {
                *cache.borrow_mut() = Some(DeckCache {
                    key: deck_cache_key(&st, 1),
                    surf,
                });
            }
        }

        {
            let weak = Rc::downgrade(&state);
            let cache_w = Rc::downgrade(&cache);
            area.set_draw_func(move |area, cr, _w, _h| {
                let (Some(st), Some(cache)) = (weak.upgrade(), cache_w.upgrade()) else {
                    return;
                };
                let st = st.borrow();
                draw_deck_cached(&st, &cache, area.scale_factor(), cr);
            });
        }

        // touch: press marks the keycap down; release on the same key fires it
        {
            let gesture = gtk4::GestureClick::new();
            let state_g = state.clone();
            let area_g = area.clone();
            gesture.connect_pressed(move |_g, _n, x, y| {
                // bind to a local FIRST: an `if let` keeps the condition's
                // temporary (the Ref borrow) alive for the whole block,
                // which collides with the borrow_mut below (boot-proven crash)
                let hit = hit_test(&state_g.borrow(), x, y);
                if let Some(hit) = hit {
                    state_g.borrow_mut().pressed = Some(hit);
                    area_g.queue_draw();
                }
            });
            let state_r = state.clone();
            let area_r = area.clone();
            let cache_r = cache.clone();
            gesture.connect_released(move |_g, _n, x, y| {
                let pressed = state_r.borrow_mut().pressed.take();
                if let Some(p) = pressed {
                    // bound before the &&: the borrow inside the condition
                    // must not outlive the expression (see pressed-handler)
                    let fires = release_fires(&state_r.borrow(), p, x, y);
                    if fires && apply_key(&state_r, p) {
                        relayout(&state_r, &area_r, &cache_r); // layer switch: new legends
                    }
                }
                area_r.queue_draw();
            });
            let state_s = state.clone();
            let area_s = area.clone();
            gesture.connect_stopped(move |_g| {
                state_s.borrow_mut().pressed = None;
                area_s.queue_draw();
            });
            area.add_controller(gesture);
        }

        container.append(&area);
        Self {
            container,
            area,
            state,
        }
    }
}

/// Tab glyph — up when the deck is stowed ("tap to raise"), down when it is
/// showing ("tap to stow").
fn tab_glyph(shown: bool) -> &'static str {
    if shown {
        "▼"
    } else {
        "▲"
    }
}

/// The deck in its bottom dock: a slide-up revealer that ALWAYS starts
/// collapsed, plus the small toggle tab at the bottom center of the view.
/// Neither view ever sprouts the deck uninvited — on the locker it would
/// cover the art before it is needed, on the greeter it is simply noise.
pub struct OskDock {
    /// the tab; the view appends it above `dock`, centered on the width
    pub tab: gtk4::Label,
    /// slide-up revealer around the deck; collapsed natural size is 0
    pub dock: gtk4::Revealer,
    /// the raw deck, for allocation diagnostics
    pub container: gtk4::Box,
}

impl OskDock {
    pub fn new(accent: Option<HexColor>, on_event: impl Fn(OskEvent) + 'static) -> Self {
        let osk = Osk::new(accent, on_event);

        let dock = gtk4::Revealer::new();
        dock.set_transition_type(gtk4::RevealerTransitionType::SlideUp);
        dock.set_transition_duration(220);
        dock.set_reveal_child(false);
        dock.set_child(Some(&osk.container));

        let tab = gtk4::Label::new(Some(tab_glyph(false)));
        tab.add_css_class("osk-tab");
        // center on whatever width the column below the content offers
        tab.set_hexpand(true);
        tab.set_halign(gtk4::Align::Center);

        // house idiom: gesture on a plain label, no GTK button theming.
        // reveals_child() is the TARGET state, so re-taps mid-animation
        // simply reverse course instead of fighting the transition.
        let gesture = gtk4::GestureClick::new();
        {
            let dock_g = dock.clone();
            let tab_g = tab.clone();
            gesture.connect_pressed(move |_g, _n, _x, _y| {
                let shown = !dock_g.reveals_child();
                dock_g.set_reveal_child(shown);
                tab_g.set_text(tab_glyph(shown));
            });
        }
        tab.add_controller(gesture);

        Self {
            tab,
            dock,
            container: osk.container,
        }
    }
}

/// Rebuild the key layout for the current layer; also (re)size the area.
fn relayout(
    state: &Rc<RefCell<DeckState>>,
    area: &gtk4::DrawingArea,
    cache: &Rc<RefCell<Option<DeckCache>>>,
) {
    let (rows, total) = {
        let st = state.borrow();
        layout_rows(st.sym_layer)
    };
    {
        let mut st = state.borrow_mut();
        st.layout = rows;
        st.total = total;
    }
    *cache.borrow_mut() = None; // geometry changed: next draw re-renders
    area.set_size_request(total.0 as i32, total.1 as i32);
    area.queue_draw();
}

/// Pure layout computation — no widgets, unit-testable.
fn layout_rows(sym_layer: bool) -> (Vec<Vec<KeyDef>>, (f64, f64)) {
    let (letters, nums) = if sym_layer {
        (SYM_LETTERS, SYM_NUMBERS)
    } else {
        (LETTERS, NUMBERS)
    };

    // rows as (left offset, entries) before the chassis pad is added
    type Entry = (String, f64, KeyKind, KeyAction);
    let mk = |s: &str| s.to_uppercase();
    let char_of = |s: &str| s.chars().next().unwrap_or('\0');
    let key = |s: &str| (mk(s), KEY_W, KeyKind::Normal, KeyAction::Char(char_of(s)));

    let mut rows: Vec<(f64, Vec<Entry>)> = Vec::new();

    // number row; BKSP rides the right wall at ↵'s exact width (x set
    // below, once the deck width is known)
    let mut r: Vec<Entry> = nums.iter().map(|k| key(k)).collect();
    r.push(("BKSP".into(), ENTER_W, KeyKind::Modifier, KeyAction::Backspace));
    rows.push((0.0, r));

    // Q row
    rows.push((
        STAGGER_Q,
        letters[0].iter().map(|k| key(k)).collect(),
    ));

    // home row, ↵ anchored at its right end
    let mut r: Vec<Entry> = letters[1].iter().map(|k| key(k)).collect();
    r.push(("↵".into(), ENTER_W, KeyKind::Modifier, KeyAction::Enter));
    rows.push((STAGGER_A, r));

    // bottom letter row. SHIFT and the layer key below it form the deck's
    // LEFT rail, flush with the number row's 1 — the letters still stair-
    // step past them. The caps LED lives on the SHIFT cap.
    let mut r: Vec<Entry> = vec![(
        "SHIFT".into(),
        SHIFT_W,
        KeyKind::Modifier,
        KeyAction::ToggleShift,
    )];
    r.extend(letters[2].iter().map(|k| key(k)));
    rows.push((0.0, r));

    // bottom bar: layer key under SHIFT; the space bar sits centered in
    // the span to its right (its x is finalized below, once the deck
    // width is known)
    let layer_lbl = if sym_layer { "ABC" } else { "?123" };
    rows.push((
        0.0,
        vec![
            (layer_lbl.into(), MOD_W, KeyKind::Modifier, KeyAction::ToggleLayer),
            ("SPACE".into(), SPACE_W, KeyKind::Space, KeyAction::Char(' ')),
        ],
    ));

    let row_w = |off: f64, es: &Vec<Entry>| {
        off + es.iter().map(|e| e.1).sum::<f64>() + GAP * es.len().saturating_sub(1) as f64
    };
    let content_w = rows
        .iter()
        .map(|(off, es)| row_w(*off, es))
        .fold(0.0_f64, f64::max);
    let n = rows.len() as f64;
    let total = (
        content_w + PAD * 2.0,
        n * KEY_H + (n - 1.0) * GAP + PAD * 2.0,
    );

    let mut layout = Vec::new();
    for (ri, (off, entries)) in rows.iter().enumerate() {
        let y = PAD + ri as f64 * (KEY_H + GAP);
        let mut x = PAD + off;
        let mut defs = Vec::new();
        for (label, w, kind, action) in entries {
            defs.push(KeyDef {
                label: label.to_string(),
                x,
                y,
                w: *w,
                kind: *kind,
                action: *action,
            });
            x += w + GAP;
        }
        layout.push(defs);
    }
    // the space bar rides centered in the span right of the layer key —
    // equal air on both sides reads as design, not an unfilled row
    let space = layout.last_mut().unwrap().last_mut().unwrap();
    space.x += (content_w - MOD_W - GAP - SPACE_W) / 2.0;
    // BKSP pins to the right wall at ↵'s exact width: the two action keys
    // bracket the deck's right edge, one row apart
    let bksp = layout[0].last_mut().unwrap();
    bksp.x = PAD + content_w - ENTER_W;
    (layout, total)
}

fn hit_test(st: &DeckState, x: f64, y: f64) -> Option<(usize, usize)> {
    for (ri, row) in st.layout.iter().enumerate() {
        for (ki, k) in row.iter().enumerate() {
            if x >= k.x && x <= k.x + k.w && y >= k.y && y <= k.y + KEY_H {
                return Some((ri, ki));
            }
        }
    }
    None
}

/// Does the release fire the pressed key? Same key: yes. A different cap:
/// no — that is a deliberate slide-off. No cap at all (a gap, the chassis
/// margin, just off the deck): yes within [`RELEASE_SLOP`] of the pressed
/// cap — fingertip drift, not intent.
fn release_fires(st: &DeckState, press: (usize, usize), x: f64, y: f64) -> bool {
    match hit_test(st, x, y) {
        Some(h) => h == press,
        None => st
            .layout
            .get(press.0)
            .and_then(|row| row.get(press.1))
            .map(|k| {
                x >= k.x - RELEASE_SLOP
                    && x <= k.x + k.w + RELEASE_SLOP
                    && y >= k.y - RELEASE_SLOP
                    && y <= k.y + KEY_H + RELEASE_SLOP
            })
            .unwrap_or(false),
    }
}

/// Apply a key action; returns true when the deck needs a relayout.
/// State is mutated and the event computed under one short borrow; the
/// handler runs AFTER the borrow drops — calling back into GTK while
/// holding a RefCell borrow re-enters the draw func and panics.
fn apply_key(state: &Rc<RefCell<DeckState>>, (ri, ki): (usize, usize)) -> bool {
    enum Emit {
        None,
        Char(char),
        Backspace,
        Enter,
    }
    let (emit, rebuild) = {
        let mut st = state.borrow_mut();
        let action = match st.layout.get(ri).and_then(|r| r.get(ki)) {
            Some(k) => k.action,
            None => return false,
        };
        match action {
            KeyAction::Char(c) => {
                let out = if st.shifted || st.shift_latched {
                    c.to_uppercase().next().unwrap_or(c)
                } else {
                    c
                };
                if st.shifted {
                    st.shifted = false; // one-shot; the latch persists
                }
                (Emit::Char(out), false)
            }
            KeyAction::Backspace => (Emit::Backspace, false),
            KeyAction::Enter => (Emit::Enter, false),
            KeyAction::ToggleShift => {
                // tap 1: one-shot · tap 2: latch (caps) · tap 3: release
                if st.shift_latched {
                    st.shift_latched = false;
                } else if st.shifted {
                    st.shifted = false;
                    st.shift_latched = true;
                } else {
                    st.shifted = true;
                }
                (Emit::None, false)
            }
            KeyAction::ToggleLayer => {
                st.sym_layer = !st.sym_layer;
                (Emit::None, true)
            }
        }
    };
    // borrow released — safe to call back into GTK/UI
    match emit {
        Emit::None => {}
        Emit::Char(c) => (state.borrow().on_event)(OskEvent::Char(c)),
        Emit::Backspace => (state.borrow().on_event)(OskEvent::Backspace),
        Emit::Enter => (state.borrow().on_event)(OskEvent::Enter),
    }
    rebuild
}

// ── drawing ───────────────────────────────────────────────────────────

/// Everything that changes the UNPRESSED deck's look — the cache key.
fn deck_cache_key(st: &DeckState, scale: i32) -> (bool, bool, bool, i32) {
    (st.sym_layer, st.shifted, st.shift_latched, scale)
}

/// Cached deck draw: blit the pre-rendered unpressed image, then paint the
/// pressed cap (if any) live on top. The image rebuilds only when the look
/// state changes (layer/shift/scale); a plain tap is one blit + one keycap.
fn draw_deck_cached(
    st: &DeckState,
    cache: &RefCell<Option<DeckCache>>,
    scale: i32,
    cr: &cairo::Context,
) {
    let key = deck_cache_key(st, scale);
    if cache.borrow().as_ref().map_or(true, |c| c.key != key) {
        match render_deck_unpressed(st, scale) {
            Some(surf) => *cache.borrow_mut() = Some(DeckCache { key, surf }),
            // surface refused (memory?): draw direct, uncached — no worse
            // than the deck's pre-cache cost, and next frame tries again
            None => return draw_deck(st, cr),
        }
    }
    {
        let cell = cache.borrow();
        let deck = cell.as_ref().expect("deck cache just built");
        cr.save().unwrap();
        // the image is device-scale px; the target context is logical
        cr.scale(1.0 / scale as f64, 1.0 / scale as f64);
        cr.set_source_surface(&deck.surf, 0.0, 0.0).unwrap();
        cr.paint().unwrap();
        cr.restore().unwrap();
    }
    if let Some((ri, ki)) = st.pressed {
        draw_key(st, cr, ri, ki, true);
    }
}

/// Render the deck with no cap pressed into a device-resolution image.
/// Pure cairo — no GTK — so the tests can drive it headless.
fn render_deck_unpressed(st: &DeckState, scale: i32) -> Option<cairo::ImageSurface> {
    let s = scale as f64;
    let img = cairo::ImageSurface::create(
        cairo::Format::ARgb32,
        (st.total.0 * s).round().max(1.0) as i32,
        (st.total.1 * s).round().max(1.0) as i32,
    )
    .ok()?;
    let cr = cairo::Context::new(&img).ok()?;
    cr.scale(s, s); // draw in logical px at device resolution
    draw_chassis(&cr, &st.pal, st.total.0, st.total.1);
    for (ri, row) in st.layout.iter().enumerate() {
        for (ki, _) in row.iter().enumerate() {
            draw_key(st, &cr, ri, ki, false);
        }
    }
    Some(img)
}

/// Uncached full draw — the fallback path when a surface can't be had, and
/// the exact pre-cache behavior.
fn draw_deck(st: &DeckState, cr: &cairo::Context) {
    draw_chassis(cr, &st.pal, st.total.0, st.total.1);
    for (ri, row) in st.layout.iter().enumerate() {
        for (ki, _) in row.iter().enumerate() {
            draw_key(st, cr, ri, ki, st.pressed == Some((ri, ki)));
        }
    }
}

/// The chassis: a near-black phosphor panel the caps sit in.
fn draw_chassis(cr: &cairo::Context, pal: &DeckPalette, tw: f64, th: f64) {
    cr.set_source_rgba(
        pal.chassis_bg.0,
        pal.chassis_bg.1,
        pal.chassis_bg.2,
        0.92,
    );
    rounded_rect(cr, 0.5, 0.5, tw - 1.0, th - 1.0, CHASSIS_R);
    cr.fill().unwrap();
    cr.set_source_rgb(
        pal.chassis_edge.0,
        pal.chassis_edge.1,
        pal.chassis_edge.2,
    );
    cr.set_line_width(1.0);
    rounded_rect(cr, 0.5, 0.5, tw - 1.0, th - 1.0, CHASSIS_R);
    cr.stroke().unwrap();
}

/// One cap, exactly as the deck paints it — shared by the cache render
/// (`pressed` forced false) and the live pressed overlay (forced true), so
/// the two paths can never drift apart.
fn draw_key(st: &DeckState, cr: &cairo::Context, ri: usize, ki: usize, pressed: bool) {
    let Some(k) = st.layout.get(ri).and_then(|r| r.get(ki)) else {
        return;
    };
    let is_shift = matches!(k.action, KeyAction::ToggleShift);
    let latched = is_shift && (st.shifted || st.shift_latched);
    let led = if is_shift {
        shift_led(st.shifted, st.shift_latched)
    } else {
        LedState::Off
    };
    draw_keycap(cr, &st.pal, k, pressed, latched, led);
}

fn rounded_rect(cr: &cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    cr.new_sub_path();
    cr.arc(x + w - r, y + r, r, -std::f64::consts::PI / 2.0, 0.0);
    cr.arc(x + w - r, y + h - r, r, 0.0, std::f64::consts::PI / 2.0);
    cr.arc(x + r, y + h - r, r, std::f64::consts::PI / 2.0, std::f64::consts::PI);
    cr.arc(x + r, y + r, r, std::f64::consts::PI, 3.0 * std::f64::consts::PI / 2.0);
    cr.close_path();
}

fn draw_keycap(
    cr: &cairo::Context,
    pal: &DeckPalette,
    k: &KeyDef,
    pressed: bool,
    latched: bool,
    led: LedState,
) {
    let (face, side) = if pressed {
        (pal.pressed_face, pal.pressed_face)
    } else if latched {
        (pal.latch_face, pal.cap_side)
    } else {
        match k.kind {
            KeyKind::Normal => (pal.cap_face, pal.cap_side),
            KeyKind::Modifier | KeyKind::Space => (pal.mod_face, pal.mod_side),
        }
    };

    if pressed {
        // key traveled down: single flush face, full accent — the
        // inversion IS the feedback
        cr.set_source_rgb(face.0, face.1, face.2);
        rounded_rect(cr, k.x, k.y, k.w, KEY_H, RADIUS);
        cr.fill().unwrap();
    } else {
        // keycap side (depth), then the top face above it
        cr.set_source_rgb(side.0, side.1, side.2);
        rounded_rect(cr, k.x, k.y, k.w, KEY_H, RADIUS);
        cr.fill().unwrap();
        cr.set_source_rgb(face.0, face.1, face.2);
        rounded_rect(cr, k.x, k.y, k.w, KEY_H - DEPTH, RADIUS);
        cr.fill().unwrap();
    }

    // hairline border
    let edge = if pressed {
        pal.pressed_text
    } else {
        pal.cap_edge
    };
    cr.set_source_rgb(edge.0, edge.1, edge.2);
    cr.set_line_width(1.0);
    rounded_rect(cr, k.x + 0.5, k.y + 0.5, k.w - 1.0, KEY_H - 1.0, RADIUS);
    cr.stroke().unwrap();

    // legend
    let is_enter = matches!(k.action, KeyAction::Enter);
    let legend = if pressed {
        pal.pressed_text
    } else if latched {
        pal.latch_legend
    } else if k.kind == KeyKind::Normal || is_enter {
        pal.legend
    } else {
        pal.legend_dim
    };
    if k.kind == KeyKind::Normal {
        cr.select_font_face("DejaVu Sans", cairo::FontSlant::Normal, cairo::FontWeight::Bold);
        cr.set_font_size(20.0);
    } else if is_enter {
        // the return arrow reads as a glyph, not a word — set it big
        cr.select_font_face("DejaVu Sans", cairo::FontSlant::Normal, cairo::FontWeight::Normal);
        cr.set_font_size(30.0);
    } else {
        cr.select_font_face("DejaVu Sans Mono", cairo::FontSlant::Normal, cairo::FontWeight::Normal);
        cr.set_font_size(13.0);
    }
    let ext = cr.text_extents(&k.label).unwrap();
    let tx = k.x + (k.w - ext.width()) / 2.0 - ext.x_bearing();
    let ty = k.y + (KEY_H - DEPTH) / 2.0 - ext.height() / 2.0 - ext.y_bearing();
    cr.set_source_rgb(legend.0, legend.1, legend.2);
    cr.move_to(tx, ty);
    let _ = cr.show_text(&k.label);

    if matches!(k.action, KeyAction::ToggleShift) {
        draw_shift_led(cr, pal, k, led);
    }
}

/// Hardware-style LED in the shift cap's top-right corner: a dark socket
/// when idle, dim-lit on a one-shot shift, burning bright with a soft
/// glow while the caps latch is stuck.
fn draw_shift_led(cr: &cairo::Context, pal: &DeckPalette, k: &KeyDef, led: LedState) {
    let cx = k.x + k.w - 13.0;
    let cy = k.y + 12.0;
    let r = 4.0;

    if led == LedState::Latched {
        cr.set_source_rgba(pal.led_on.0, pal.led_on.1, pal.led_on.2, 0.30);
        cr.arc(cx, cy, r + 4.5, 0.0, std::f64::consts::TAU);
        cr.fill().unwrap();
    }
    // socket
    cr.set_source_rgb(pal.led_off.0, pal.led_off.1, pal.led_off.2);
    cr.arc(cx, cy, r, 0.0, std::f64::consts::TAU);
    cr.fill().unwrap();
    // lit state
    let lit = match led {
        LedState::Off => None,
        LedState::Once => Some(pal.led_once),
        LedState::Latched => Some(pal.led_on),
    };
    if let Some(c) = lit {
        cr.set_source_rgb(c.0, c.1, c.2);
        cr.arc(cx, cy, r - 1.0, 0.0, std::f64::consts::TAU);
        cr.fill().unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_is_bounded() {
        for sym in [false, true] {
            let (rows, (tw, th)) = layout_rows(sym);
            assert_eq!(rows.len(), 5);
            assert!(tw > 0.0 && th > 0.0);
            for row in &rows {
                for k in row {
                    assert!(k.x >= -0.01, "key left of deck: {}", k.x);
                    assert!(k.x + k.w <= tw + 0.01, "key past right edge");
                    assert!(k.y + super::KEY_H <= th + 0.01);
                }
            }
        }
    }

    #[test]
    fn rows_cascade_in_the_typewriter_stagger() {
        let (rows, _) = layout_rows(false);
        // the letter rows step right: 1, Q, A, Z — A never under Q, Z
        // never under A. SHIFT (row 3, key 0) stays on the flush-left rail.
        assert!(rows[1][0].x > rows[0][0].x, "Q must sit right of the number row");
        assert!(rows[2][0].x > rows[1][0].x, "A must sit right of Q — never under it");
        assert!(rows[3][1].x > rows[2][0].x, "Z must sit right of A");
        assert_eq!(rows[3][0].x, rows[0][0].x, "SHIFT belongs on the left rail");
    }

    #[test]
    fn action_keys_anchor_the_classic_edges() {
        let (rows, _) = layout_rows(false);
        assert_eq!(rows[0].last().unwrap().action, KeyAction::Backspace);
        assert_eq!(rows[2].last().unwrap().label, "↵");
        assert_eq!(rows[2].last().unwrap().action, KeyAction::Enter);
        // BKSP and ↵ hug the deck's right edge, and at the same width the
        // pair reads as one column of action keys
        let right = rows
            .iter()
            .flatten()
            .map(|k| k.x + k.w)
            .fold(0.0_f64, f64::max);
        for k in [rows[0].last().unwrap(), rows[2].last().unwrap()] {
            assert!((k.x + k.w - right).abs() < 0.01, "{} not flush right", k.label);
            assert_eq!(k.w, ENTER_W, "{} must be ↵-wide", k.label);
        }
    }

    #[test]
    fn layer_key_under_shift_and_space_centered() {
        let (rows, _) = layout_rows(false);
        assert_eq!(rows[3][0].label, "SHIFT");
        let bottom = &rows[4];
        assert_eq!(bottom[0].x, rows[3][0].x, "layer key belongs under SHIFT");
        assert_eq!(bottom[0].label, "?123");
        let (sym, _) = layout_rows(true);
        assert_eq!(sym[4][0].label, "ABC");
        // the space bar is centered in the span right of the layer key:
        // the air before it equals the air after it
        let right = rows
            .iter()
            .flatten()
            .map(|k| k.x + k.w)
            .fold(0.0_f64, f64::max);
        let space = bottom.last().unwrap();
        let air_left = space.x - (bottom[0].x + bottom[0].w + GAP);
        let air_right = right - (space.x + space.w);
        assert!(
            (air_left - air_right).abs() < 0.01,
            "space not centered: {air_left} vs {air_right}"
        );
        assert_eq!(space.w, SPACE_W);
    }

    #[test]
    fn shift_stays_wide_but_pulls_z_into_the_as_gap() {
        let (rows, _) = layout_rows(false);
        // SHIFT keeps a modifier-width cap (not shrunk toward a letter key)
        assert!(rows[3][0].w >= 72.0, "SHIFT too narrow: {}", rows[3][0].w);
        // yet Z lands closer than a full key from A — inside the A–S gap,
        // not under S
        let (a, s, z) = (rows[2][0].x, rows[2][1].x, rows[3][1].x);
        assert!(z > a, "Z must sit right of A");
        assert!(z < s, "Z must not reach under S");
        assert!(z - a < KEY_W, "Z drifted a full key from A");
    }

    #[test]
    fn shift_led_reads_the_latch_state() {
        assert_eq!(shift_led(false, false), LedState::Off);
        assert_eq!(shift_led(true, false), LedState::Once);
        assert_eq!(shift_led(false, true), LedState::Latched);
        assert_eq!(shift_led(true, true), LedState::Latched);
    }

    #[test]
    fn deck_palette_follows_the_accent() {
        let near = |a: (f64, f64, f64), b: (f64, f64, f64)| {
            a.0 - b.0 < 0.001
                && b.0 - a.0 < 0.001
                && a.1 - b.1 < 0.001
                && b.1 - a.1 < 0.001
                && a.2 - b.2 < 0.001
                && b.2 - a.2 < 0.001
        };
        let stock = DeckPalette::new(None);
        assert!(near(
            stock.legend,
            HexColor::parse("#2bd96b").unwrap().rgb()
        ));
        let amber = HexColor {
            r: 0xff,
            g: 0xb0,
            b: 0x00,
        };
        let pal = DeckPalette::new(Some(amber));
        assert_eq!(pal.legend, amber.rgb());
        // a press inverts the cap: bright face, near-black text
        assert!(pal.pressed_face.0 > pal.pressed_text.0);
        // the latch LED burns brighter than the one-shot state
        assert!(pal.led_on.0 > pal.led_once.0);
    }

    #[test]
    fn symbols_layer_covers_every_ascii_special() {
        // same shape as the letter rows, or the geometry drifts
        for (sym, base) in SYM_LETTERS.iter().zip(LETTERS.iter()) {
            assert_eq!(sym.len(), base.len());
        }
        // every ASCII printable the base layer can't emit (, . / ; can)
        // must be reachable on the symbols layer, and nothing outside
        // ASCII snuck in — a lock screen types passwords, not prose
        let base_specials = ",./;";
        let ascii_specials = "!\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~";
        let mut sym: Vec<char> = Vec::new();
        let mut push = |s: &str| {
            assert_eq!(s.chars().count(), 1, "sym keys are single characters: {s}");
            sym.push(s.chars().next().unwrap());
        };
        for s in SYM_NUMBERS {
            push(s);
        }
        for row in SYM_LETTERS.iter() {
            for s in row.iter() {
                push(s);
            }
        }
        for c in ascii_specials.chars() {
            if base_specials.contains(c) {
                continue;
            }
            assert!(sym.contains(&c), "'{c}' unreachable on the symbols layer");
        }
    }

    #[test]
    fn shift_legend_is_uppercase_but_action_emits_lowercase() {
        let (rows, _) = layout_rows(false);
        let q = &rows[1][0]; // rows[0] is the number row
        assert_eq!(q.label, "Q");
        assert_eq!(q.action, KeyAction::Char('q'));
    }

    #[test]
    fn tab_points_up_when_stowed_and_down_when_raised() {
        assert_eq!(tab_glyph(false), "▲"); // deck hidden: raise it
        assert_eq!(tab_glyph(true), "▼"); // deck showing: stow it
    }

    // a DeckState over the pure layout, for hit/release testing
    fn deck_with(pressed: Option<(usize, usize)>) -> DeckState {
        let (layout, total) = layout_rows(false);
        DeckState {
            shifted: false,
            shift_latched: false,
            sym_layer: false,
            pressed,
            layout,
            total,
            pal: DeckPalette::new(None),
            on_event: Box::new(|_| ()),
        }
    }

    #[test]
    fn release_forgives_finger_drift_but_honors_slide_off() {
        let st = deck_with(None);
        let (q, w) = (&st.layout[1][0], &st.layout[1][1]); // Q and its neighbor W
        let press = (1, 0);
        let cy = q.y + KEY_H / 2.0;

        // dead-center release: fires (the always-did case)
        assert!(release_fires(&st, press, q.x + q.w / 2.0, cy));
        // drifted into the 6px gap between caps: still fires
        assert!(release_fires(&st, press, q.x + q.w + 3.0, cy));
        // drifted into the 6px gap above the row: still fires
        assert!(release_fires(&st, press, q.x + q.w / 2.0, q.y - 3.0));
        // off the deck's left edge, within slop: fires
        assert!(release_fires(&st, press, q.x - 10.0, cy));
        // onto the neighbor cap: a deliberate slide-off — cancels
        assert!(!release_fires(&st, press, w.x + w.w / 2.0, cy));
        // past the slop margin with no cap under the finger: cancels
        assert!(!release_fires(&st, press, q.x - 13.0, cy));
        assert!(!release_fires(&st, press, q.x + q.w / 2.0, q.y - 13.0));
        // below the bottom row (space bar), within slop vs past it
        let bottom = st.layout.last().unwrap().last().unwrap();
        let space = (4, st.layout[4].len() - 1);
        let bx = bottom.x + bottom.w / 2.0;
        assert!(release_fires(&st, space, bx, bottom.y + KEY_H + 10.0));
        assert!(!release_fires(&st, space, bx, bottom.y + KEY_H + 13.0));
    }

    #[test]
    fn cached_image_never_bakes_the_pressed_cap() {
        // headless cairo, no GTK: render the cached image with a key held
        // down — the image must show the RESTING cap; the live overlay must
        // show the bright accent inversion
        let st = deck_with(Some((1, 1)));
        let mut img = render_deck_unpressed(&st, 1).expect("deck surface");

        // sample inside the cap's face, clear of the legend glyph
        let k = &st.layout[1][1];
        let (sx, sy) = ((k.x + 12.0) as i32, (k.y + 12.0) as i32);
        let resting = pixel_sum(&mut img, sx, sy);
        assert!(
            resting < 100,
            "resting cap face must stay near-black: {resting}"
        );

        let mut live = cairo::ImageSurface::create(
            cairo::Format::ARgb32,
            st.total.0 as i32,
            st.total.1 as i32,
        )
        .expect("overlay surface");
        let cr = cairo::Context::new(&live).expect("overlay context");
        draw_key(&st, &cr, 1, 1, true);
        drop(cr);
        let pressed = pixel_sum(&mut live, sx, sy);
        assert!(
            pressed > 300,
            "pressed overlay must invert to full accent: {pressed}"
        );
    }

    /// channel sum at (x, y) of an opaque ARGB32 surface — coarse, but the
    /// resting face (~39) and the accent inversion (~367) are far apart
    fn pixel_sum(img: &mut cairo::ImageSurface, x: i32, y: i32) -> u32 {
        let stride = img.stride() as usize;
        let data = img.data().expect("surface data");
        let i = y as usize * stride + x as usize * 4;
        u32::from(data[i]) + u32::from(data[i + 1]) + u32::from(data[i + 2])
    }
}
